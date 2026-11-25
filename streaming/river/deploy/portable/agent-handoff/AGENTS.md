# River deployment handoff

Scope: deploying or testing River from this extracted bundle only.
Read DEPLOYMENT.md and UPGRADE-FROM-M1230.md in the current deployment bundle before running starter scripts. Historical profile notes are context, not current release pins.
Use BUILD-MANIFEST.json for the exact image tag and ID; verify SHA256SUMS.
Use release.json, when present, for recorded release-channel metadata.
Read SETTINGS.md for source, decoder, cache, logging and TLS controls. Treat profiles as starting points, not established optima.

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
- Startup precedence is JSON then ENV. Live dedicated API changes win during the process lifetime; confirm persistence and restart behavior for the setting being changed.
- Use bounded source-read diagnostics for one operation, then disable them; check evicted/partial.
- Compare identical images/regions, driver, output size/format and resource budgets.
- Keep cold, learned-source cold-object, repeated, frame-arrival and browser-paint timings separate.
- Report the deployed image ID, preserved settings, intentional changes, checks and remaining issues.

## Release channels and image retention

Stable, LTS and Latest refer to one image and one source implementation. The m1231 release was explicitly packaged without a new qualification run. Do not reintroduce frozen channel forks. Qualify exact digests with matched pixels and performance comparisons; never infer speedups from larger caches. Keep historical measurements labeled with their original build and settings. Update aliases, release metadata, Compose defaults, documentation, retention and deployment metadata together. Retire superseded images after inspecting references; preserve configurations, volumes, exports and evidence.
