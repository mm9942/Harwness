import { Container } from "@cloudflare/containers";

// Example agent container. This is the reference implementation for adding a
// new agent container: define a `Container` subclass here, register its
// binding in `wrangler.jsonc`, and add an entry to `src/agents/registry.ts`.
export class ExampleAgent extends Container<Env> {
	// Port the container listens on (default: 8080)
	defaultPort = 8080;
	// Time before container sleeps due to inactivity (default: 30s)
	sleepAfter = "2m";
	// Environment variables passed to the container
	envVars = {
		MESSAGE: "Hello from the example agent!",
	};

	// Optional lifecycle hooks
	override onStart() {
		console.log("ExampleAgent container successfully started");
	}

	override onStop() {
		console.log("ExampleAgent container successfully shut down");
	}

	override onError(error: unknown) {
		console.log("ExampleAgent container error:", error);
	}
}