# Containers Starter

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/cloudflare/templates/tree/main/containers-template)

![Containers Template Preview](https://imagedelivery.net/_yJ02hpOMj_EnGvsU2aygw/5aba1fb7-b937-46fd-fa67-138221082200/public)

<!-- dash-content-start -->

This is a [Container](https://developers.cloudflare.com/containers/) starter template.

It demonstrates basic Container configuration, launching and routing to individual container, load balancing over multiple container, running basic hooks on container status changes.

<!-- dash-content-end -->

Outside of this repo, you can start a new project with this template using [C3](https://developers.cloudflare.com/pages/get-started/c3/) (the `create-cloudflare` CLI):

```bash
npm create cloudflare@latest -- --template=cloudflare/templates/containers-template
```

## Getting Started

First, run:

```bash
npm install
# or
yarn install
# or
pnpm install
# or
bun install
```

Then run the development server (using the package manager of your choice):

```bash
npm run dev
```

Open [http://localhost:8787](http://localhost:8787) with your browser to see the result.

You can start editing your Worker by modifying `src/index.ts` and you can start
editing your Container by editing the content of `container_src`.

## Deploying To Production

| Command          | Action                                |
| :--------------- | :------------------------------------ |
| `npm run deploy` | Deploy your application to Cloudflare |

## Learn More

To learn more about Containers, take a look at the following resources:

- [Container Documentation](https://developers.cloudflare.com/containers/) - learn about Containers
- [Container Class](https://github.com/cloudflare/containers) - learn about the Container helper class

Your feedback and contributions are welcome!

## Neuen Agenten-Container hinzufügen

Jeder Agenten-Container folgt demselben Muster: eine Container-Klasse, eine
DO-Namespace-Bindung, ein Registry-Eintrag und ein eigenes Dockerfile. So fügst
du einen neuen Agenten hinzu:

1. **Container-Klasse** in `src/agents/<name>.ts` anlegen — extends
   `Container<Env>`, setzt `defaultPort`, `sleepAfter` und `envVars`. Beispiel:
   `src/agents/example-agent.ts`.
2. **Dockerfile** unter `containers/<name>/Dockerfile` anlegen — Kopie des
   Go-Multi-Stage-Setups aus `container_src` (analog zum Root-`Dockerfile`),
   `EXPOSE 8080`. `container_src` bleibt unverändert.
3. **Registry** in `src/agents/registry.ts` erweitern — Eintrag mit `name`,
   `readBinding` (liefert die DO-Bindung aus `Env`) und `imageKey` in
   `AGENT_REGISTRY`.
4. **wrangler.jsonc** erweitern:
   - `containers`-Eintrag mit `class_name`, `image: "./containers/<name>/Dockerfile"`
     und `max_instances`.
   - DO-Binding im `durable_objects.bindings`-Block (`<NAME>` als Upper-Snake-Case).
   - Klasse in die `new_sqlite_classes`-Liste der Migration `v1` aufnehmen.
5. **Typen**: `worker-configuration.d.ts` um die neue Binding ergänzen
   (`<NAME>: DurableObjectNamespace<import("./src/agents/<name>").<Class>>`).

Der neue Agent ist dann über `GET /agents/<name>/:id` erreichbar.
