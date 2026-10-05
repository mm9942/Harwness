import { DurableObject } from "cloudflare:workers";

const GATEWAY_ID = "mias-lab";
const DEFAULT_MODEL = "@cf/moonshotai/kimi-k2.7-code";
const DEFAULT_CACHE_TTL_SECONDS = 1800;
const CACHE_KEY_VERSION = "v1";
const QUEUE_TIMEOUT_MS = 15_000;
const MAX_QUEUED_PER_MODEL = 32;

const MODEL_ALIASES = {
  "kimi-k2.7-code": "@cf/moonshotai/kimi-k2.7-code",
  "kimi-k2.6": "@cf/moonshotai/kimi-k2.6",
  "glm-5.3": "@cf/zai-org/glm-5.3",
  "glm-5.3-flash": "@cf/zai-org/glm-5.3-flash",
  "glm-5.2": "@cf/zai-org/glm-5.2",
  "gpt-oss-120b": "@cf/openai/gpt-oss-120b",
  "gpt-oss-20b": "@cf/openai/gpt-oss-20b",
};

const ALLOWED_MODELS = new Set([
  DEFAULT_MODEL,
  "@cf/moonshotai/kimi-k2.6",
  "@cf/zai-org/glm-5.3",
  "@cf/zai-org/glm-5.3-flash",
  "@cf/zai-org/glm-5.2",
  "@cf/openai/gpt-oss-120b",
  "@cf/openai/gpt-oss-20b",
  "@cf/deepseek-ai/deepseek-v4-flash-0731",
]);

const corsHeaders = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET,POST,OPTIONS",
  "access-control-allow-headers": [
    "authorization",
    "content-type",
    "x-api-key",
    "x-client-request-id",
    "x-harw-session",
    "x-harw-agent",
    "x-harw-role",
    "x-harw-goal",
    "x-harw-wave",
    "x-harw-cache-affinity",
  ].join(","),
  "access-control-expose-headers": [
    "retry-after",
    "x-harw-ai-model",
    "x-harw-ai-gateway",
    "x-harw-affinity",
    "x-harw-cache-mode",
    "x-harw-concurrency-limit",
    "x-harw-queue-depth",
  ].join(","),
};

function json(data, status = 200, extraHeaders = {}) {
  return new Response(JSON.stringify(data), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      ...corsHeaders,
      ...extraHeaders,
    },
  });
}

function cleanHeader(request, name, max = 96) {
  const value = request.headers.get(name);
  if (!value) return "";
  return value
    .replace(/[^\x20-\x7e]/g, "")
    .trim()
    .slice(0, max);
}

function normalizeModel(model) {
  const value = String(model || DEFAULT_MODEL).trim();
  return MODEL_ALIASES[value] || value;
}

function concurrencyLimitForModel(model) {
  if (
    model.includes("kimi-k2.7") ||
    model.includes("kimi-k2.6") ||
    model.includes("glm-5.3") ||
    model.includes("glm-5.2")
  ) {
    return 2;
  }
  if (model.includes("gpt-oss-120b") || model.includes("deepseek-v4")) {
    return 3;
  }
  return 6;
}

function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

function canonicalize(value) {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (!value || typeof value !== "object") return value;
  const out = {};
  for (const key of Object.keys(value).sort()) {
    const child = value[key];
    if (child !== undefined) out[key] = canonicalize(child);
  }
  return out;
}

async function sha256Hex(text) {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(text),
  );
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

function textFromContent(content) {
  if (content == null) return "";
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return JSON.stringify(content);
  return content
    .map((part) => {
      if (typeof part === "string") return part;
      if (typeof part?.text === "string") return part.text;
      if (typeof part?.input_text === "string") return part.input_text;
      return "";
    })
    .filter(Boolean)
    .join("\n");
}

async function resolveAffinity(request, model, messages) {
  const explicit = cleanHeader(request, "x-harw-cache-affinity", 128);
  if (explicit) return explicit;

  const session = cleanHeader(request, "x-harw-session", 64);
  const agent = cleanHeader(request, "x-harw-agent", 64);
  if (session || agent) {
    return `harw:${session || "-"}:${agent || "-"}`;
  }

  const system = messages.find((message) => message?.role === "system");
  const firstUser = messages.find((message) => message?.role === "user");
  const seed = [
    model,
    textFromContent(system?.content).slice(0, 4096),
    textFromContent(firstUser?.content).slice(0, 4096),
  ].join("\u0000");
  return `auto:${(await sha256Hex(seed)).slice(0, 24)}`;
}

function shouldUseExactResponseCache(body) {
  if (body.stream === true) return false;
  if (Array.isArray(body.tools) && body.tools.length > 0) return false;
  if (body.tool_choice && body.tool_choice !== "none") return false;
  if (body.n && Number(body.n) !== 1) return false;
  return true;
}

function cacheTtlForRequest(body) {
  const explicit = Number(body?.metadata?.harw_cache_ttl);
  if (Number.isFinite(explicit)) {
    return Math.max(0, Math.min(86_400, Math.floor(explicit)));
  }
  return DEFAULT_CACHE_TTL_SECONDS;
}

async function exactCacheKey(model, body) {
  const normalized = cloneJson(body);
  delete normalized.stream;
  delete normalized.user;
  delete normalized.metadata;
  normalized.model = model;
  return `${CACHE_KEY_VERSION}:${await sha256Hex(JSON.stringify(canonicalize(normalized)))}`;
}

function modelMetadata(request, model, affinity) {
  const pick = (name, fallback = "-") => cleanHeader(request, name, 64) || fallback;
  return {
    session: pick("x-harw-session", affinity.slice(0, 64)),
    agent: pick("x-harw-agent"),
    role: pick("x-harw-role", "worker"),
    goal: pick("x-harw-goal"),
    wave: pick("x-harw-wave"),
  };
}

function mapUpstreamBody(body, model) {
  const allowed = [
    "messages",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "temperature",
    "top_p",
    "seed",
    "stop",
    "presence_penalty",
    "frequency_penalty",
    "response_format",
    "reasoning_effort",
    "chat_template_kwargs",
    "max_tokens",
    "max_completion_tokens",
  ];
  const upstream = { model, stream: body.stream === true };
  for (const key of allowed) {
    if (body[key] !== undefined && body[key] !== null) upstream[key] = body[key];
  }
  if (!Array.isArray(upstream.messages)) upstream.messages = [];
  return upstream;
}

function normalizeChatCompletion(answer, model) {
  if (answer?.object === "chat.completion" && Array.isArray(answer?.choices)) {
    return answer;
  }
  const message = answer?.choices?.[0]?.message || {
    role: "assistant",
    content:
      answer?.response ??
      answer?.result?.response ??
      answer?.text ??
      "",
  };
  return {
    id: `chatcmpl-${crypto.randomUUID()}`,
    object: "chat.completion",
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [
      {
        index: 0,
        message,
        finish_reason: answer?.choices?.[0]?.finish_reason || "stop",
      },
    ],
    usage: answer?.usage,
  };
}

function presentedKey(request) {
  const authorization = request.headers.get("authorization") || "";
  if (authorization.toLowerCase().startsWith("bearer ")) {
    return authorization.slice(7).trim();
  }
  return request.headers.get("x-api-key") || "";
}

async function digestForCompare(value) {
  return new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)),
  );
}

async function authorized(request, env) {
  if (!env.GATEWAY_API_KEY) return false;
  const [presented, expected] = await Promise.all([
    digestForCompare(presentedKey(request)),
    digestForCompare(env.GATEWAY_API_KEY),
  ]);
  if (presented.length !== expected.length) return false;
  let diff = 0;
  for (let i = 0; i < presented.length; i += 1) diff |= presented[i] ^ expected[i];
  return diff === 0;
}

export class ModelLane extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.env = env;
    this.inflight = 0;
    this.waiters = [];
    this.coalesced = new Map();
  }

  async acquire(limit) {
    if (this.inflight < limit) {
      this.inflight += 1;
      return;
    }
    if (this.waiters.length >= MAX_QUEUED_PER_MODEL) {
      throw new Error("queue_full");
    }

    await new Promise((resolve, reject) => {
      const waiter = { resolve, reject, timer: null };
      waiter.timer = setTimeout(() => {
        const index = this.waiters.indexOf(waiter);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(new Error("queue_timeout"));
      }, QUEUE_TIMEOUT_MS);
      this.waiters.push(waiter);
    });
  }

  release() {
    this.inflight = Math.max(0, this.inflight - 1);
    const waiter = this.waiters.shift();
    if (waiter) {
      clearTimeout(waiter.timer);
      // Reserve the released permit before waking the waiter so a newly
      // arriving request cannot steal the slot between resolve() and resume.
      this.inflight += 1;
      waiter.resolve();
    }
  }

  trackedStream(stream) {
    const reader = stream.getReader();
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      this.release();
    };

    return new ReadableStream({
      async pull(controller) {
        try {
          const { value, done } = await reader.read();
          if (done) {
            finish();
            controller.close();
            return;
          }
          controller.enqueue(value);
        } catch (error) {
          finish();
          controller.error(error);
        }
      },
      async cancel(reason) {
        finish();
        await reader.cancel(reason);
      },
    });
  }

  async runNonStreaming(request, body, model, affinity, cacheKey, cacheTtl) {
    const existing = this.coalesced.get(cacheKey);
    if (existing) {
      const stored = await existing;
      return json(stored.payload, stored.status, stored.headers);
    }

    const task = this.execute(request, body, model, affinity, cacheKey, cacheTtl, false)
      .then(async (response) => {
        const payload = await response.json();
        const headers = Object.fromEntries(response.headers.entries());
        return { payload, status: response.status, headers };
      })
      .finally(() => this.coalesced.delete(cacheKey));

    this.coalesced.set(cacheKey, task);
    const stored = await task;
    return json(stored.payload, stored.status, stored.headers);
  }

  async execute(request, body, model, affinity, cacheKey, cacheTtl, stream) {
    const limit = concurrencyLimitForModel(model);
    try {
      await this.acquire(limit);
    } catch (error) {
      const kind = String(error?.message || error);
      return json(
        { error: { type: "rate_limit_error", code: kind, message: "mias-lab model lane is busy" } },
        429,
        {
          "retry-after": kind === "queue_timeout" ? "5" : "2",
          "x-harw-concurrency-limit": String(limit),
          "x-harw-queue-depth": String(this.waiters.length),
        },
      );
    }

    let streamOwnsPermit = false;
    const cacheEnabled = shouldUseExactResponseCache(body);
    const metadata = modelMetadata(request, model, affinity);

    try {
      const upstream = mapUpstreamBody(body, model);
      const answer = await this.env.AI.run(model, upstream, {
        gateway: {
          id: GATEWAY_ID,
          skipCache: !cacheEnabled,
          cacheTtl: cacheEnabled ? cacheTtl : undefined,
          cacheKey: cacheEnabled ? cacheKey : undefined,
          metadata,
        },
        extraHeaders: {
          "x-session-affinity": affinity,
          "cf-aig-collect-log-payload": "false",
        },
      });

      const headers = {
        ...corsHeaders,
        "x-harw-ai-model": model,
        "x-harw-ai-gateway": GATEWAY_ID,
        "x-harw-affinity": affinity.slice(0, 128),
        "x-harw-cache-mode": cacheEnabled ? "exact+prefix" : "prefix-only",
        "x-harw-concurrency-limit": String(limit),
        "x-harw-queue-depth": String(this.waiters.length),
      };

      if (stream || answer instanceof ReadableStream) {
        if (!(answer instanceof ReadableStream)) {
          return json(
            { error: { message: "upstream did not return a stream" } },
            502,
            headers,
          );
        }
        streamOwnsPermit = true;
        return new Response(this.trackedStream(answer), {
          status: 200,
          headers: {
            ...headers,
            "content-type": "text/event-stream; charset=utf-8",
            "cache-control": "no-cache",
          },
        });
      }

      return json(normalizeChatCompletion(answer, model), 200, headers);
    } catch (error) {
      const message = String(error?.message || error);
      const capacity = /3040|capacity|rate.?limit|too many requests|\b429\b/i.test(message);
      return json(
        {
          error: {
            type: capacity ? "rate_limit_error" : "api_error",
            message: capacity ? "Workers AI capacity pressure" : "Workers AI request failed",
          },
        },
        capacity ? 429 : 502,
        capacity ? { "retry-after": "20" } : {},
      );
    } finally {
      if (!streamOwnsPermit) this.release();
    }
  }

  async fetch(request) {
    if (request.method !== "POST") return new Response("Method Not Allowed", { status: 405 });
    const body = await request.json();
    const model = normalizeModel(body.model);
    const affinity = await resolveAffinity(request, model, body.messages || []);
    const cacheTtl = cacheTtlForRequest(body);
    const cacheKey = await exactCacheKey(model, body);

    if (shouldUseExactResponseCache(body)) {
      return this.runNonStreaming(request, body, model, affinity, cacheKey, cacheTtl);
    }
    return this.execute(request, body, model, affinity, cacheKey, cacheTtl, body.stream === true);
  }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (request.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: corsHeaders });
    }

    if (request.method === "GET" && (url.pathname === "/" || url.pathname === "/health")) {
      return json({ ok: true, service: "mias-lab", gateway: GATEWAY_ID });
    }

    if (!(await authorized(request, env))) {
      return json({ error: { message: "Unauthorized" } }, 401);
    }

    if (request.method === "GET" && url.pathname === "/v1/models") {
      return json({
        object: "list",
        data: [...ALLOWED_MODELS].map((id) => ({ id, object: "model", owned_by: "cloudflare" })),
      });
    }

    if (request.method !== "POST" || url.pathname !== "/v1/chat/completions") {
      return json({ error: { message: "Not found" } }, 404);
    }

    let body;
    try {
      body = await request.json();
    } catch {
      return json({ error: { message: "Invalid JSON" } }, 400);
    }

    const model = normalizeModel(body.model);
    if (!ALLOWED_MODELS.has(model)) {
      return json({ error: { message: `Model not allowed: ${model}` } }, 400);
    }
    if (!Array.isArray(body.messages)) {
      return json({ error: { message: "messages must be an array" } }, 400);
    }

    body.model = model;
    const id = env.MODEL_LANES.idFromName(model);
    const lane = env.MODEL_LANES.get(id);
    const forwarded = new Request("https://model-lane.internal/run", {
      method: "POST",
      headers: request.headers,
      body: JSON.stringify(body),
    });
    return lane.fetch(forwarded);
  },
};
