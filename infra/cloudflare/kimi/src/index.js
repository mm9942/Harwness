const DEFAULT_MODEL = "@cf/zai-org/glm-5.3-flash";
const GLM53_MODEL = "@cf/zai-org/glm-5.3";
const GLM53_ROUTE = { binding: "AI", gateway: "more-exessive-work" };

function caps(context_window, input_modalities, supports_reasoning, supports_parallel_tool_calls = false) {
  return {
    context_window,
    input_modalities,
    supports_reasoning,
    supports_tools: true,
    supports_parallel_tool_calls,
  };
}

const TEXT = ["text"];
const TEXT_IMAGE = ["text", "image"];

const MODEL_CAPABILITIES = {
  "@cf/moonshotai/kimi-k2.7-code": caps(262144, TEXT_IMAGE, true, true),
  "@cf/qwen/qwen2.5-coder-32b-instruct": caps(32768, TEXT, false),
  "@cf/openai/gpt-oss-120b": caps(131072, TEXT, true),
  "@cf/openai/gpt-oss-20b": caps(131072, TEXT, true),
  "@cf/deepseek-ai/deepseek-r1-distill-qwen-32b": caps(32768, TEXT, true),
  "@cf/qwen/qwen3-30b-a3b-fp8": caps(32768, TEXT, true),
  "@cf/qwen/qwq-32b": caps(32768, TEXT, true),
  "@cf/zai-org/glm-5.2": caps(262144, TEXT, true, true),
  "@cf/zai-org/glm-5.3": caps(1310720, TEXT, true, true),
  "@cf/zai-org/glm-5.3-flash": caps(1310720, TEXT_IMAGE, true, true),
  "@cf/zai-org/glm-4.7-flash": caps(131072, TEXT, true),
  "@cf/moonshotai/kimi-k2.6": caps(262144, TEXT_IMAGE, true),
  "@cf/deepseek-ai/deepseek-v4-flash-0731": caps(1310720, TEXT, true),
  "@cf/deepseek-ai/deepseek-v4-pro-0813": caps(1048576, TEXT, true),
  "@cf/nvidia/nemotron-3-120b-a12b": caps(256000, TEXT, true),
  "@cf/google/gemma-4-26b-a4b-it": caps(256000, TEXT_IMAGE, true),
  "@cf/mistralai/mistral-small-3.1-24b-instruct": caps(128000, TEXT, false),
  "@cf/qwen/qwen3.8-27b": caps(262144, TEXT_IMAGE, true),
  "@cf/meta/llama-3.3-70b-instruct-fp8-fast": caps(24000, TEXT, false),
  "@cf/ibm-granite/granite-4.0-h-micro": caps(131000, TEXT, false),
  "@cf/meta/llama-4-scout-17b-16e-instruct": caps(131000, TEXT_IMAGE, false),
};

const MODELS = Object.keys(MODEL_CAPABILITIES);

// previous_response_id support is best-effort compatibility only (Harw is
// stateless): isolate-local, TTL + LRU bounded, and each entry stores just
// its delta plus a parent pointer so memory grows linearly, not quadratically.
const STORE_MAX_ENTRIES = 2000;
const STORE_TTL_MS = 30 * 60 * 1000;
const HISTORY_MAX_DEPTH = 500;
const responseStore = new Map();
const historyStore = new Map();

function storeSet(store, key, value) {
  store.delete(key);
  store.set(key, { value, expires: Date.now() + STORE_TTL_MS });
  while (store.size > STORE_MAX_ENTRIES) {
    store.delete(store.keys().next().value);
  }
}

function storeGet(store, key) {
  const entry = store.get(key);
  if (!entry) return undefined;
  if (entry.expires < Date.now()) {
    store.delete(key);
    return undefined;
  }
  // Refresh LRU position.
  store.delete(key);
  store.set(key, entry);
  return entry.value;
}

// Rebuilds the full message list by walking parent pointers; a broken chain
// (evicted/foreign isolate) degrades to "unknown response", as before.
function loadHistory(responseId) {
  const chain = [];
  let id = responseId;
  while (id && chain.length < HISTORY_MAX_DEPTH) {
    const node = storeGet(historyStore, id);
    if (!node) return [];
    chain.push(node.delta);
    id = node.parent;
  }
  return chain.reverse().flat();
}

// Maps each inference call onto one of the Worker's AI bindings, each
// paired with one authenticated AI Gateway. All bindings resolve to the same
// Workers AI account/quota (rotating them does not add capacity), but each
// gateway pairing gets its own independent caching/rate-limiting/retry
// policy and its own request log, which is the actual reason for the split.
const AI_ROUTES = [
  { binding: "AI", gateway: "default" },
  { binding: "AI", gateway: "claw" },
  { binding: "AI", gateway: "clawd" },
  { binding: "AI", gateway: "cyberclaw" },
  { binding: "AI", gateway: "whisper" },
  { binding: "AI", gateway: "sgh-chatbot" },
  { binding: "AI", gateway: "agentic" },
  { binding: "AI", gateway: "more-exessive-work" },
  { binding: "AI", gateway: "new-worker-of-the-day" },
  { binding: "AI", gateway: "new-worker-of-the-day-5" },
  { binding: "AI", gateway: "new-worker-of-the-week" },
  { binding: "AI", gateway: "new-worker-of-the-month" },
  { binding: "AI", gateway: "new-worker-of-the-year" },
  { binding: "AI", gateway: "new-worker-of-the-century" },
  { binding: "AI", gateway: "new-worker-of-the-millennium" },
  { binding: "AI", gateway: "new-worker-of-the-eon" },
  { binding: "AI", gateway: "new-worker-of-the-eternity" },
  { binding: "AI", gateway: "new-worker-of-the-universe" },
  { binding: "AI", gateway: "new-worker-of-the-multiverse" },
]

// Per-isolate, best-effort route health. Each agent has one preferred route
// (prefix-cache locality). While that route is cooling down the agent uses the
// next healthy route in a fixed order, and returns home once the cooldown ends.
const ROUTE_STATE = AI_ROUTES.map(() => ({
  inflight: 0,
  streaming: 0,
  latencyEwma: 0,
  streamOpenEwma: 0,
  firstTokenEwma: 0,
  streamTotalEwma: 0,
  cooldownUntil: 0,
  consecutive429: 0,
  consecutive5xx: 0,
}));

// GLM 5.3 deliberately gets its own isolated Workers AI binding and AI
// Gateway. It never participates in the ordinary route pool, so a failure
// stays attributable to the dedicated GLM 5.3 path instead of being masked
// by failover onto one of the ordinary bindings.
const GLM53_ROUTE_STATE = {
  inflight: 0,
  streaming: 0,
  latencyEwma: 0,
  streamOpenEwma: 0,
  firstTokenEwma: 0,
  streamTotalEwma: 0,
  cooldownUntil: 0,
  consecutive429: 0,
  consecutive5xx: 0,
};

function ewma(previous, sample) {
  return previous ? previous * 0.8 + sample * 0.2 : sample;
}

// Sampling/format parameters forwarded verbatim when the client sets them.
const PASSTHROUGH_PARAMS = [
  "response_format",
  "seed",
  "stop",
  "presence_penalty",
  "frequency_penalty",
];

// Account-wide capacity errors must not be retried across routes (they all
// share one Workers AI quota); the client is told to back off instead.
// Harw ignores retry-after values above 60 s.
const CAPACITY_RETRY_AFTER_S = 20;

const HARW_HEADERS = [
  "x-harw-session",
  "x-harw-agent",
  "x-harw-role",
  "x-harw-goal",
  "x-harw-wave",
];

const TELEMETRY_HEADERS = [
  "x-harw-ai-model",
  "x-harw-ai-route",
  "x-harw-ai-gateway",
  "x-harw-ai-latency-ms",
  "x-harw-ai-stream-open-ms",
  "x-harw-ai-cache-tokens",
  "x-harw-ai-affinity",
  "x-harw-ai-failovers",
];

const corsHeaders = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET,POST,OPTIONS",
  "access-control-allow-headers":
    "authorization,content-type,x-session-affinity,x-client-request-id,x-api-key," +
    HARW_HEADERS.join(","),
  "access-control-expose-headers":
    TELEMETRY_HEADERS.concat(["retry-after"]).join(","),
};

function now() {
  return Math.floor(Date.now() / 1000);
}

function makeId(prefix) {
  return prefix + "_" + crypto.randomUUID().replaceAll("-", "");
}

function withJsonHeaders(headers) {
  return Object.assign({}, corsHeaders, headers);
}

function json(data, status = 200, extraHeaders = {}) {
  return new Response(JSON.stringify(data, null, 2), {
    status,
    headers: withJsonHeaders(
      Object.assign(
        { "content-type": "application/json; charset=utf-8" },
        extraHeaders,
      ),
    ),
  });
}

function headerValue(request, name, max = 128) {
  const value = request?.headers?.get(name);
  return value ? value.trim().slice(0, max) : "";
}

async function sha256Hex(text) {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(text),
  );
  return Array.from(
    new Uint8Array(digest),
    (b) => b.toString(16).padStart(2, "0"),
  ).join("");
}

// Derives a stable per-agent key. Explicit client identity wins; otherwise
// model + first system + first user message identify one agent conversation
// (constant across its turns, distinct between agents of a wave).
async function resolveAffinity(request, model, messages) {
  const explicit =
    headerValue(request, "x-session-affinity") ||
    [
      headerValue(request, "x-harw-session"),
      headerValue(request, "x-harw-agent"),
    ]
      .filter(Boolean)
      .join(":");

  const system = messages.find((m) => m.role === "system");
  const user = messages.find((m) => m.role === "user");

  const seed =
    explicit ||
    [
      model,
      textFromContent(system?.content).slice(0, 4096),
      textFromContent(user?.content).slice(0, 4096),
    ].join("\u0000");

  const hex = await sha256Hex(model + "\u0000" + seed);

  return {
    key: explicit || "aff_" + hex.slice(0, 16),
    hash: parseInt(hex.slice(0, 8), 16),
  };
}

// AI Gateway accepts at most five metadata entries (string/number/bool).
function buildGatewayMetadata(request, affinity, role) {
  const pick = (name, fallback) =>
    headerValue(request, name, 64) || fallback;

  return {
    session: pick("x-harw-session", affinity),
    agent: pick("x-harw-agent", "anon"),
    role: pick("x-harw-role", role),
    goal: pick("x-harw-goal", "-"),
    wave: pick("x-harw-wave", "-"),
  };
}

function routeCandidates(hash) {
  const count = AI_ROUTES.length;
  const preferred = hash % count;
  const order = Array.from(
    { length: count },
    (_, i) => (preferred + i) % count,
  );

  const t = Date.now();
  const healthy = order.filter(
    (i) => ROUTE_STATE[i].cooldownUntil <= t,
  );

  return healthy.length ? healthy : order;
}

// Documented Cloudflare codes decide the reaction:
//   2003 AI Gateway rate limited     -> route-local, one failover
//   3040 Workers AI out of capacity  -> shared quota, no failover
//   3036 account limited             -> billing/allocation, no retry
//   3007 request timeout / 5xx       -> one attempt on another route
const ERROR_CODE_KINDS = {
  2003: "route_429",
  3040: "capacity",
  3036: "account",
  3007: "server",
  3043: "server",
};

function errorCodeOf(error) {
  for (const candidate of [
    error?.code,
    error?.cause?.code,
    error?.errorCode,
    error?.status,
  ]) {
    const code = Number(candidate);
    if (Number.isInteger(code) && code > 0) {
      return code;
    }
  }

  const match = /\b(2003|3007|3036|3040|3043)\b/.exec(
    String(error?.message || ""),
  );

  return match ? Number(match[1]) : null;
}

function classifyAiError(error) {
  const code = errorCodeOf(error);

  if (code && ERROR_CODE_KINDS[code]) {
    return ERROR_CODE_KINDS[code];
  }

  if (code === 429) {
    return "capacity";
  }

  if (code >= 500 && code <= 599) {
    return "server";
  }

  const message = String(error?.message || error);

  if (/capacity/i.test(message)) {
    return "capacity";
  }

  // An unexplained rate limit is treated as shared-capacity pressure: backing
  // off is safer than walking all routes into the same limit.
  if (/rate.?limit|too many requests|\b429\b/i.test(message)) {
    return "capacity";
  }

  if (
    /internal (server )?error|service unavailable|bad gateway|timed? ?out/i.test(
      message,
    )
  ) {
    return "server";
  }

  return "fatal";
}

function markRouteSuccess(state, latencyMs) {
  state.latencyEwma = ewma(state.latencyEwma, latencyMs);
  state.consecutive429 = 0;
  state.consecutive5xx = 0;
}

// Wraps an upstream SSE stream so inflight/streaming counters stay raised
// until the stream really ends and first-token/total timings are recorded.
function trackStream(stream, state, started, route, affinity) {
  const reader = stream.getReader();
  let sawFirstChunk = false;
  let finished = false;

  const finish = (status, error) => {
    if (finished) return;

    finished = true;
    state.inflight = Math.max(0, state.inflight - 1);
    state.streaming = Math.max(0, state.streaming - 1);
    state.streamTotalEwma = ewma(
      state.streamTotalEwma,
      Date.now() - started,
    );

    if (status === "completed") {
      state.consecutive429 = 0;
      state.consecutive5xx = 0;
    } else if (status === "failed" && error) {
      const kind = classifyAiError(error);
      markRouteFailure(state, kind);

      logEvent("route.stream_failed", {
        binding: route.binding,
        gateway: route.gateway,
        kind,
        code: errorCodeOf(error),
        affinity,
        error: String(error?.message || error),
      });
    }
  };

  return new ReadableStream({
    async pull(controller) {
      try {
        const { value, done } = await reader.read();

        if (done) {
          finish("completed");
          controller.close();
          return;
        }

        if (!sawFirstChunk) {
          sawFirstChunk = true;
          state.firstTokenEwma = ewma(
            state.firstTokenEwma,
            Date.now() - started,
          );
        }

        controller.enqueue(value);
      } catch (error) {
        finish("failed", error);
        controller.error(error);
      }
    },

    cancel(reason) {
      // Client cancellation is not evidence that the upstream route is unhealthy.
      finish("cancelled");
      return reader.cancel(reason);
    },
  });
}

function markRouteFailure(state, kind) {
  const t = Date.now();

  if (kind === "route_429") {
    state.consecutive429 += 1;
    state.cooldownUntil =
      t +
      Math.min(
        60000,
        5000 * 2 ** (state.consecutive429 - 1),
      );
  } else if (kind === "server") {
    state.consecutive5xx += 1;
    state.cooldownUntil =
      t + Math.min(15000, 2000 * state.consecutive5xx);
  }
}

class ClientRequestError extends Error {
  constructor(message, code = "invalid_request_error") {
    super(message);
    this.name = "ClientRequestError";
    this.code = code;
  }
}

class CapacityError extends Error {}
class AccountLimitError extends Error {}

// Runs one inference on the agent's sticky gateway route; fails over across at most
// three gateway IDs for route-local problems (gateway 429, 5xx). Shared Workers AI
// capacity/account limits are not retried across gateways.
async function runOnRoutes(env, model, requestBody, info) {
  // GLM 5.3 is intentionally isolated from the normal route pool: no route
  // hashing, no alternate AI binding, no gateway failover.
  if (model === GLM53_MODEL) {
    return runDedicatedGlm53Route(env, model, requestBody, info);
  }

  let lastError;
  let failovers = 0;

  for (const index of routeCandidates(info.hash).slice(0, 3)) {
    const route = AI_ROUTES[index];
    const state = ROUTE_STATE[index];
    const ai = env[route.binding];

    if (!ai) {
      lastError = new Error(
        'AI binding "' +
          route.binding +
          '" is not configured on this Worker',
      );
      continue;
    }

    state.inflight += 1;
    let handedToStream = false;
    const started = Date.now();

    try {
      const answer = await ai.run(model, requestBody, {
        // Keep AI Gateway metadata logs but never persist prompt/response bodies.
        // skipCache disables the Gateway's full-response cache only; Workers AI
        // prefix caching remains active through x-session-affinity.
        gateway: {
          id: route.gateway,
          skipCache: true,
          metadata: info.metadata,
        },

        extraHeaders: {
          "x-session-affinity": info.affinity,
          "cf-aig-collect-log-payload": "false",
        },
      });

      const elapsed = Date.now() - started;

      if (answer instanceof ReadableStream) {
        handedToStream = true;
        state.streaming += 1;
        state.streamOpenEwma = ewma(
          state.streamOpenEwma,
          elapsed,
        );

        return {
          answer: trackStream(
            answer,
            state,
            started,
            route,
            info.affinity,
          ),
          route,
          failovers,
          streamOpenMs: elapsed,
        };
      }

      markRouteSuccess(state, elapsed);

      return {
        answer,
        route,
        failovers,
        latencyMs: elapsed,
      };
    } catch (error) {
      const kind = classifyAiError(error);
      markRouteFailure(state, kind);

      logEvent("route.failed", {
        binding: route.binding,
        gateway: route.gateway,
        kind,
        code: errorCodeOf(error),
        affinity: info.affinity,
        error: String(error?.message || error),
      });

      if (kind === "capacity") {
        throw new CapacityError(
          String(error?.message || error),
        );
      }

      if (kind === "account") {
        throw new AccountLimitError(
          String(error?.message || error),
        );
      }

      if (kind === "fatal") {
        throw error;
      }

      lastError = error;
      failovers += 1;
    } finally {
      if (!handedToStream) {
        state.inflight -= 1;
      }
    }
  }

  throw lastError;
}

// Dedicated GLM 5.3 path: single binding, single gateway, no failover. If
// this fails, the failure is never masked by trying an ordinary route.
async function runDedicatedGlm53Route(env, model, requestBody, info) {
  const route = GLM53_ROUTE;
  const state = GLM53_ROUTE_STATE;
  const ai = env[route.binding];
  if (!ai) {
    throw new Error('Dedicated GLM 5.3 AI binding "' + route.binding + '" is not configured on this Worker');
  }
  state.inflight += 1;
  let handedToStream = false;
  const started = Date.now();
  try {
    logEvent("glm53.route.start", { model, binding: route.binding, gateway: route.gateway, affinity: info.affinity });
    const answer = await ai.run(model, requestBody, {
      gateway: { id: route.gateway, skipCache: true, metadata: info.metadata },
      extraHeaders: {
        "x-session-affinity": info.affinity,
        "cf-aig-collect-log-payload": "false",
      },
    });
    const elapsed = Date.now() - started;
    if (answer instanceof ReadableStream) {
      handedToStream = true;
      state.streaming += 1;
      state.streamOpenEwma = ewma(state.streamOpenEwma, elapsed);
      logEvent("glm53.route.stream_open", {
        model,
        binding: route.binding,
        gateway: route.gateway,
        affinity: info.affinity,
        stream_open_ms: elapsed,
      });
      return {
        answer: trackStream(answer, state, started, route, info.affinity),
        route,
        failovers: 0, // deliberately zero: GLM 5.3 is not allowed to fail over
        streamOpenMs: elapsed,
      };
    }
    markRouteSuccess(state, elapsed);
    logEvent("glm53.route.success", {
      model,
      binding: route.binding,
      gateway: route.gateway,
      affinity: info.affinity,
      latency_ms: elapsed,
    });
    return { answer, route, failovers: 0, latencyMs: elapsed };
  } catch (error) {
    const kind = classifyAiError(error);
    markRouteFailure(state, kind);
    logEvent("glm53.route.failed", {
      model,
      binding: route.binding,
      gateway: route.gateway,
      affinity: info.affinity,
      kind,
      code: errorCodeOf(error),
      duration_ms: Date.now() - started,
      error: String(error?.message || error),
    });
    if (kind === "capacity") throw new CapacityError(String(error?.message || error));
    if (kind === "account") throw new AccountLimitError(String(error?.message || error));
    // IMPORTANT: no fallback onto AI/work/MoreWork/etc.
    throw error;
  } finally {
    if (!handedToStream) state.inflight = Math.max(0, state.inflight - 1);
  }
}

function cachedTokensOf(usage) {
  return (
    usage?.prompt_tokens_details?.cached_tokens ??
    usage?.input_tokens_details?.cached_tokens ??
    0
  );
}

// Streams only know their open time when headers are sent; completion latency
// and cached tokens are unknown then, so those headers are omitted rather than
// reported as 0 (which would claim "no cache hit").
function telemetryHeaders(result, cachedTokens) {
  const headers = {
    "x-harw-ai-model": result.model,
    "x-harw-ai-route": result.route.binding,
    "x-harw-ai-gateway": result.route.gateway,
    "x-harw-ai-affinity": result.affinity,
    "x-harw-ai-failovers": String(result.failovers),
  };

  if (result.streamOpenMs !== undefined) {
    headers["x-harw-ai-stream-open-ms"] =
      String(result.streamOpenMs);
  } else {
    headers["x-harw-ai-latency-ms"] =
      String(result.latencyMs);
    headers["x-harw-ai-cache-tokens"] =
      String(cachedTokens || 0);
  }

  return headers;
}

function getPresentedApiKey(req) {
  const auth = req.headers.get("authorization") || "";

  if (auth.toLowerCase().startsWith("bearer ")) {
    return auth.slice(7).trim();
  }

  return req.headers.get("x-api-key") || "";
}

function isPublicPath(method, path) {
  return (
    method === "OPTIONS" ||
    (method === "GET" &&
      (path === "/" || path === "/health"))
  );
}

function requireAuth(req, env, path) {
  if (isPublicPath(req.method, path)) {
    return null;
  }

  if (!env.GATEWAY_API_KEY) {
    return json(
      {
        error: {
          message: "Worker missing GATEWAY_API_KEY secret",
        },
      },
      500,
    );
  }

  if (getPresentedApiKey(req) !== env.GATEWAY_API_KEY) {
    return json(
      {
        error: {
          message: "Unauthorized",
        },
      },
      401,
    );
  }

  return null;
}

function textFromContent(content) {
  if (content == null) return "";

  if (typeof content === "string") {
    return content;
  }

  if (!Array.isArray(content)) {
    return JSON.stringify(content);
  }

  return content
    .map((part) => {
      if (typeof part === "string") {
        return part;
      }

      if (typeof part?.text === "string") {
        return part.text;
      }

      if (typeof part?.input_text === "string") {
        return part.input_text;
      }

      if (
        part?.type === "input_image" ||
        part?.type === "image_url"
      ) {
        const image = part.image_url ?? part.url;
        const url =
          typeof image === "string"
            ? image
            : image?.url;

        return url
          ? "[image:" + url.slice(0, 512) + "]"
          : "[image]";
      }

      if (part?.content != null) {
        return textFromContent(part.content);
      }

      return "";
    })
    .filter(Boolean)
    .join("\n");
}

// Reasoning text from a Responses "reasoning" input item: raw content wins,
// summary is the fallback (encrypted_content cannot be used by these models).
function reasoningTextFromItem(item) {
  const raw = textFromContent(item.content);
  return raw || textFromContent(item.summary);
}

// Upstream models emit reasoning as `reasoning_content`; some newer
// OpenAI-compatible backends use `reasoning` instead.
function reasoningOf(message) {
  if (!message) return "";

  if (typeof message.reasoning_content === "st --- TRUNCATED --- 71,670 chars