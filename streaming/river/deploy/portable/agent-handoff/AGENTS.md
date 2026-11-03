# River deployment handoff

Scope: deploying or testing River from this extracted bundle only.
Read ORGANIZATIONAL-DEPLOYMENT.txt before running the starter scripts.
Use BUILD-MANIFEST.json for the exact image tag and ID; verify SHA256SUMS.
Use release.json, when present, for recorded release-channel metadata.
For the 32 CPU / approximately 62 GB nearby-S3 deployment, read POD-32CORE-TESTING.md.
Treat its profile as a tuning starting point, not an established optimum.

## Preserve the existing installation

- Inventory the current River workload, image, mounts, environment and API settings first.
- Preserve every existing S3 source ID, endpoint, bucket, region, prefix, addressing mode,
  credentials/provider, session-token mechanism, TLS trust and Secret/service-account reference.
- Preserve config/cache PVCs or volumes and their existing mount paths, project names,
  custom configuration paths, image-data mounts, routes, ports and resource settings.
- Preserve WebP settings, read-policy limits, retry choices and other current customizations.
- Do not replace an existing .env with an example or start a fresh Compose project over an upgrade.
- Do not delete/recreate S3 connections to tune performance. IDs also identify learned source state
  and versioned technical-cache entries.
- Do not import an example as a whole configuration: a document with sources: [] erases sources.
- Do not delete volumes, clear existing caches, modify original imagery or upload image bundles.
- Keep rollback metadata and private configuration backups outside this distributable bundle.
  Configuration exports contain credentials; source-list summaries are not restorable backups.

## Deliberate tuning and evidence

- For an existing deployment, GET read-policy and PUT the same settings with only mode changed
  to adaptive. Preserve its other values unless an additional change is explicitly recorded.
- The supplied JSON and environment examples are FRESH-INSTALL presets, never merge commands.
- Persisted API settings override environment fallbacks. Confirm effective settings after restart.
- Use bounded source-read diagnostics for one operation, then disable them; check evicted/partial.
- Compare identical images/regions, driver, output size/format and resource budgets.
- Keep cold, learned-source cold-object, repeated, frame-arrival and browser-paint timings separate.
- Report the deployed image ID, preserved settings, intentional changes, checks and remaining issues.

## Release channels and image retention

Latest is the current experiment; Stable is any verified improvement that is stable; LTS advances on a verified concrete >=10% improvement in a substantial matched workload. Verify pixels and investigate regressions. Read deploy/releases.json from the owning source for current channel IDs; old checkpoint numbers in historical reports are not current pins. Update aliases, release metadata, benchmark policy, retention pins and deployment metadata together. Retire superseded images promptly, preserving existing configuration/cache volumes, exports and evidence. Do not force-remove externally referenced images.
