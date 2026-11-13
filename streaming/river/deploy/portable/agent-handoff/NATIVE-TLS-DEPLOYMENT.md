# River deployment handoff for native TLS

Read AGENTS.md and ORGANIZATIONAL-DEPLOYMENT.txt first. Preserve the existing
project or namespace, S3 connections, environment, data, cache and config
volumes. Keep the current image digest and deployment manifest for rollback.

## What's in this build

- Native TLS serves the existing River API on a separate listener, default
  port 8443. The regular HTTP listener remains available for internal traffic
  and readiness probes.
- Mount either a PEM certificate chain and key or a PFX file and optional
  password file. River reads these at startup. Restart after rotating files.
- Select any combination of HTTP/1.1, HTTP/2 and HTTP/3. The default is
  HTTP/1.1 and HTTP/2. HTTP/3 uses UDP, and TCP and UDP can use the same port.
- Stable/LTS inherit m1185's JSON, environment and live API precedence for S3
  connections and enabled drivers. TLS configuration is applied on startup.
  Stable/LTS also retain automatic Grok routing for eligible indexed zooms.

## Docker Compose

1. Verify the ZIP and the entries in SHA256SUMS. Load the included image
   with `docker load -i river-image.tar.gz`. Identify the image from
   BUILD-MANIFEST.json. Preserve the existing Compose project name.
2. Mount the certificate directory read-only. The included compose.tls.yaml
   maps it to /run/river-tls and publishes TCP and UDP on local port 18443.
   Copy the settings below to the existing service environment or to
   river-config.env next to compose.yaml. Keep that file out of source control.
3. Update only the River backend. For the portable bundle, run
   `docker compose -f compose.yaml -f compose.tls.yaml up -d --no-deps river`.
   If you already use a GPU or other override, keep it in the command.

PEM settings:

```dotenv
RIVER_TLS_CERT_FILE=/run/river-tls/tls.crt
RIVER_TLS_KEY_FILE=/run/river-tls/tls.key
RIVER_TLS_PROTOCOLS=http/1.1,h2,h3
RIVER_HTTP3_PUBLIC_PORT=18443
```

For PFX, replace both PEM variables with:

```dotenv
RIVER_TLS_PFX_FILE=/run/river-tls/river.pfx
RIVER_TLS_PFX_PASSWORD_FILE=/run/river-tls/password
```

RIVER_TLS_PFX_PASSWORD_FILE points to the mounted file containing the password
on its first line. Omit the variable for an empty PFX password. Use only one
certificate format. Keep passwords and private keys out of JSON, Compose YAML,
logs and image layers. JSON supports the same settings via the tls object
documented in the OpenAPI configuration schema. ENV overrides JSON at startup.

## Kubernetes

Mount a Secret into the backend at /run/river-tls. A kubernetes.io/tls Secret
provides tls.crt and tls.key. A generic Secret can provide river.pfx and
password. Set the same ENV paths in the Pod. Add named TCP and UDP ports to
the Service if both transports are enabled:

```yaml
ports:
  - name: https
    port: 443
    targetPort: 8443
    protocol: TCP
  - name: http3
    port: 443
    targetPort: 8443
    protocol: UDP
```

Set RIVER_HTTP3_PUBLIC_PORT=443 for this mapping. Ensure the external load
balancer forwards UDP as well as TCP for native HTTP/3. Keep the existing
internal readiness probe on HTTP port 8080 and avoid publicly exposing that
port when the public API should use HTTPS only. With HTTP/3 alone, expose UDP
and use an explicit HTTP/3 client. Browser discovery via Alt-Svc needs an
initial HTTPS TCP connection. The progressive viewer uses WebSockets over
HTTP/1.1 or HTTP/2. Select at least one of them for that viewer.

## Verification and rollback

- Check the running image digest and the existing /health probe.
- Use the deployment's DNS name and a trusted CA. With a curl build that
  supports the chosen protocols, run `curl --http1.1 https://HOST/health`,
  `curl --http2 -w '%{http_version}\n' https://HOST/health`, and
  `curl --http3-only -w '%{http_version}\n' https://HOST/health`. Confirm
  the negotiated versions. For a private CA, pass `--cacert` with its PEM
  file. For HTTP/3, check the external UDP path as well as local health.
- From the actual viewer origin, verify CORS, tile-info, a raster tile and a
  progressive WebSocket. Confirm existing storage sources and driver
  availability through their API endpoints without exporting credentials.
- For rollback, restore the previous image and its TLS settings or Compose
  override, recreate only the backend, and verify health and a representative
  tile. Preserve all config, cache and data volumes.
