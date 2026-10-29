---
title: 'E13.1 Sweep — `id-effect deploy k8s|helm`: restricted-by-default manifests and a Helm chart, validated in CI: r14-s-deploy-k8s'
slug: r14-s-deploy-k8s
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'dx.deploy#6 (row dx.deploy 88 → 100 once r14-p-toolchain promotes the row, D3; documented cap 89 until then): Kubernetes manifests and a Helm chart from the tool. At HEAD there is no `deploy` command (`Command`, crates/id_effect_tool/src/cli.rs:31: New, Doctor, Migrate, Dev, Generate, Rpc); the Kubernetes story is a hand-written Deployment snippet in book crates/id_effect/book/src/part6/ch34-00-dx-ship.md:130-163 next to the generated Kamal config (templates/service/config/deploy.yml.template). New: `id-effect deploy k8s [--path DIR] [--out DIR] [--image IMAGE] [--replicas N] [--namespace NS]` writes `deploy/k8s/{deployment,service,pdb,kustomization}.yaml` for a `service` project; `id-effect deploy helm [--path DIR] [--out DIR]` writes `deploy/helm/<name>/` (Chart.yaml, values.yaml, templates/{_helpers.tpl,deployment,service,pdb}.yaml) with the same defaults as values. Project name from Cargo.toml, port 8080 and uid 10001 from the generated Dockerfile (templates/common/Dockerfile.template: `useradd --uid 10001`, `EXPOSE 8080`). A non-service project exits 1 naming the template; an existing output file is never overwritten unless `--force`.'
  - 'The manifests carry the drain contract the book documents (ch34-00:130-163): RollingUpdate `maxUnavailable: 0`, `maxSurge: 1`; readiness probe `/ready`, liveness `/health`; `preStop` sleep 5 s; `terminationGracePeriodSeconds` = preStop + `SHUTDOWN_TIMEOUT_SECS` + 10 (45 by default); a PodDisruptionBudget when replicas > 1. Helm templates use `{{ .Values.… }}` with spaces, which the tool''s `substitute` keeps verbatim (crates/id_effect_tool/src/render.rs:416-427), so `{{name}}` is the only tool placeholder in them.'
  - 'Secure by default (ADR 0013): Pod Security "restricted" out of the box: `runAsNonRoot: true`, `runAsUser: 10001`, `readOnlyRootFilesystem: true` (an `emptyDir` on /tmp), `allowPrivilegeEscalation: false`, `capabilities.drop: [ALL]`, `seccompProfile: RuntimeDefault`, `automountServiceAccountToken: false`, resource requests and limits set; secrets only through `secretKeyRef` (the generated `.env.example` keys become references, never literal values); the image must carry a tag or digest (`latest` and a bare name exit 1). No flag weakens these; a user edits the YAML.'
  - 'Tests: crates/id_effect_tool/tests/deploy_k8s.rs `deploy_k8s_golden_output` (golden files under tests/golden/deploy/k8s/), `deploy_helm_golden_output` (tests/golden/deploy/helm/), `deploy_refuses_non_service_project`, `deploy_refuses_latest_or_untagged_image`, `deploy_never_overwrites_without_force`, `manifests_meet_pod_security_restricted` (parses the YAML and checks every field above), `grace_period_covers_prestop_and_shutdown_timeout`, `helm_templates_survive_substitution`; scripts/k8s-validate.sh (generates a `service` with `--local-workspace`, runs both commands, `helm lint`, `helm template … | kubeconform -strict -summary`, `kubeconform -strict` on deploy/k8s; kubeconform and helm pinned releases checked against their sha256, as docs.yml pins mdBook) with `--self-test`.'
  - 'CI: new job `deploy-manifests` in .github/workflows/ci.yml running `bash scripts/k8s-validate.sh`, added to the `ci-pass` needs list.'
  - 'Book: crates/id_effect/book/src/part6/ch34-00-dx-ship.md, the "**Kubernetes.**" part of "## Deploy" (:130-163) rewritten around `id-effect deploy k8s` / `deploy helm` (the drain explanation kept), plus a short "Pod security" paragraph. Example crates/id_effect_tool/examples/040_deploy_k8s.rs (renders the manifests for a temp project and prints them).'
  - 'Tier: integration (id_effect_tool, unchanged). No new Rust dependency: YAML is rendered from templates, and the field checks in the tests read the rendered text (schema validity is kubeconform''s job in CI). Secure by default: as above; no new network surface. Coverage: `cargo llvm-cov nextest -p id_effect_tool --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_tool --test deploy_k8s`; `bash scripts/k8s-validate.sh --self-test && bash scripts/k8s-validate.sh`; `bash scripts/check-workflows.sh`; `cargo clippy -p id_effect_tool --all-targets --all-features -- -D warnings`'
  - 'Owns: `crates/id_effect_tool/src/deploy.rs` (new), `crates/id_effect_tool/templates/deploy/**` (new), `crates/id_effect_tool/src/template.rs` (the `deploy/*` entries of `FILES`; append-only), `crates/id_effect_tool/src/cli.rs` (the `Deploy` variant of `Command` and its arm in `run`), `crates/id_effect_tool/src/lib.rs` (`pub mod deploy;`; append-only), `crates/id_effect_tool/tests/deploy_k8s.rs`, `crates/id_effect_tool/tests/golden/deploy/**`, `crates/id_effect_tool/examples/040_deploy_k8s.rs`, `scripts/k8s-validate.sh` (new), `.github/workflows/ci.yml` (job `deploy-manifests`; the `ci-pass` needs list append-only), `crates/id_effect/book/src/part6/ch34-00-dx-ship.md` (the "**Kubernetes.**" part of "## Deploy" only), append-only: `CHANGELOG.md`'
---

# r14-s-deploy-k8s

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r14-s-tool-generate (wave 2; same `cli.rs` `Command`, `template.rs` `FILES`,
`lib.rs`, and ch34-00). Blocks r14-p-toolchain (wave 5 promotes `dx.deploy`). Same-wave neighbours:
r14-s-dx-docs-lint takes `render.rs` and the template Cargo.toml / README / ci.yml templates (never
`templates/deploy/**`, `cli.rs` or `template.rs`); r14-f-h3spec also appends to the `ci-pass` needs
list of ci.yml (one line each, so the merge is mechanical; the coordinator orders the two).

Defaults the owner may overturn: (1) CI validates with kubeconform and `helm lint` / `helm template`,
not a kind cluster: schema and lint catch what the check asks for in under a minute; a kind smoke that
applies the chart and waits for `/ready` is a follow-up (r14-p-toolchain may add it to promote the row).
(2) Manifests target the Kubernetes version kubeconform is pinned to (the oldest supported upstream
minor at EXECUTE). (3) Plain manifests plus a Helm chart; no operator.
