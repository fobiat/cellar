# Container deployments

Cellar is the PID 1 supervisor in these examples. The image named by the
Compose or Kubernetes template must provide `cellar-entrypoint`, the s&box
dedicated server, Wine on Linux, and the server's gamemode packages. The root
`Dockerfile` builds the Cellar layer and documents that image boundary.

The examples keep game data and logs on persistent volumes, expose `/healthz`
for liveness and `/readyz` for serving readiness, and give Cellar 60 seconds to
send the engine's graceful `quit` command before a forced container stop.

For Docker Compose or Swarm:

```sh
docker compose -f deploy/docker-compose.yml up -d
```

The same file is accepted by Swarm after replacing the image with the final
server image and adding the registry credentials required by the cluster.

For Kubernetes, create the secret referenced by the manifest before applying it:

```sh
kubectl create secret generic cellar-secrets \
  --from-literal=CELLAR_DATABASE_URL='mysql://user:password@db/cellar' \
  --from-literal=CELLAR_WEB_PASSWORD_HASH='paste-the-output-of-cellar-hash-password' \
  --from-literal=CELLAR_API_TOKEN='a-long-random-read-only-token'
kubectl apply -f deploy/kubernetes.yaml
```

Do not commit the generated secret or put credentials in `cellar.toml`.
