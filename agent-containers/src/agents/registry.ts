import type { Container } from "@cloudflare/containers";

// Minimal structural type for the pieces of a Durable Object namespace that
// the `/agents` route needs. Keeps the registry class-agnostic (no variance
// issues between differently typed container classes).
export interface AgentBinding {
	idFromName(name: string): DurableObjectId;
	get(id: DurableObjectId): {
		fetch(request: Request): Promise<Response>;
	};
}

// A registered agent container entry.
//
// `name` is the identifier used in the `GET /agents/:name/:id` route.
// `readBinding` resolves the Durable Object binding for the agent class from
// the worker's environment.
export interface AgentRegistryEntry {
	name: string;
	description?: string;
	readBinding: (env: Env) => AgentBinding;
}

// Central registry of all agent containers exposed via the `/agents` routes.
// Add a new entry here whenever a new agent container class is registered.
const registry: Record<string, AgentRegistryEntry> = {
	"example-agent": {
		name: "example-agent",
		description:
			"Example agent container demonstrating the generic per-agent container pattern.",
		readBinding: (env) => env.EXAMPLE_AGENT,
	},
};

/**
 * Look up an agent container entry by its route name.
 *
 * @param name The agent name as used in the URL, e.g. "example-agent".
 * @returns The matching registry entry, or `undefined` if unknown.
 */
export function findAgent(name: string): AgentRegistryEntry | undefined {
	return registry[name];
}