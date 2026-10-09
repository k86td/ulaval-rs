# brio-rs — implementation plan

A Rust client for the Brio Education app API, built for two consumers: an
interactive CLI/TUI and a long-running daemon reached over Tailscale.

Status: planning complete, implementation not started.
Source material: `docs/*.har` (untracked — see `.gitignore`), `docs/openapi-brio.yaml`.

---

## 1. Decisions

These were settled during design review. Each entry records the choice and the
reason, so a future reader can tell a deliberate decision from an accident.

| # | Decision | Reason |
|---|---|---|
| D1 | Consumers are a CLI **and** an unattended daemon | Daemon implies durable auth; CLI implies cheap cold start |
| D2 | Auth bootstrap by **cookie-jar import**; renewal by `GET /auth/oauth2/authorize/` | Verified: that one GET with `session` + `session-app-pr` returns a fresh token in the URL fragment, no Entra hop, no MFA. Scripting the Entra login is impossible — `amr:["pwd","mfa"]` means MFA is enforced |
| D3 | **Brio app surface only** in v1 | Brio is the system of record for coursework and holds the timetable, grades and grade scale; monPortail's unique data is registrar-only. Partner API needs an integrator token we don't have |
| D4 | One crate, service modules, workspace retained | ~6 services share the entire envelope/common-type vocabulary; splitting forces a core crate in version lockstep for zero gain. Workspace lets `lib/monportail-rs` land later |
| D5 | Hand-written types, validated against **sanitized HAR fixtures** | No public spec exists for the app API. The 184 captured response bodies are the only ground truth, and they let tests run without burning a 15-minute token |
| D6 | Service accessor + request builder + `.send()` | 64 id fields and many optional query params; `idutilisateur` appears on 28 calls and should default to the authenticated user |
| D7 | **Proactive timer** refresh, plus check-before-use when `expires_at - now < 60s` | Token lives 899s. The timer covers the daemon; the pre-flight check covers CLI cold start, where no timer has run |
| D8 | Session in `$XDG_STATE_HOME/brio-rs/session.json`, mode **0600** | Headless-friendly; `keyring` wants D-Bus/Secret Service and degrades badly on a server |
| D9 | `chrono`; `DateTime<FixedOffset>` / `NaiveDateTime` / `NaiveTime` | 2348 of 2429 timestamps are RFC3339 with variable subsecond precision — parsed out of the box. The 77 zoneless ones stay zoneless rather than assume a zone |
| D10 | **French** type and field names, snake_cased | `serde(rename_all = "camelCase")` maps the whole crate mechanically. Translating needs a hand-typed `rename` per field — hundreds of literals that fail only at runtime |
| D11 | Phantom-typed **opaque** `Id<T>` | 64 distinct id fields; per-kind newtypes would be 40+ types. No format validation: real values include legacy `okNN`, `koNNN`, `bre1`, bare `"1"` |
| D12 | Every wire enum **preserves unknown values** | `ena2-sites-client` is at 3.43.0 with ~40 `EPIC_*` feature flags. A strict enum turns Brio's next deploy into a daemon outage |
| D13 | Realtime **out of v1**; poll instead | WebSocket handshake was captured but the HAR recorded no frames, so channel names are unknown. Brio's own SPA polls at 300s |
| D14 | **Read-only** v1 | The two captured writes are a PDF regen and `CompleterContenuCmd`, which mutates your real academic progress record. Not worth daemon risk |
| D15 | `Page<T>` + `.all()`, no `Stream` | Largest real collection is 138 rows. `Stream` costs a dep and `StreamExt` at every call site for nothing |

### Parked, not cancelled

- **OpenAPI discovery.** `/version.json` proves per-service specs exist internally
  (`ena2-sites-client`, `ena2-forums-client`, …). Probing `/sites/openapi.json`
  and friends is worth a separate session; if real specs turn up they supersede D5.
- **monPortail.** Auth is a standard Entra public-client refresh grant, so it is
  tractable — just a second mechanism. Lands as `lib/monportail-rs`.
- **Realtime.** Needs a fresh capture with WebSocket frame recording.
- **Write endpoints.** Behind an explicit opt-in, after the read path is proven.

### Standing constraints

- **The session file is a credential, not a cache.** The cookies carry
  `Max-Age=315360000` and grant full account access. Mode 0600, never logged,
  never committed, never sent anywhere but `*.brioeducation.ca`.
- **Responses contain other people's personal data.**
  `/sites/sites/:id/apprenants` returned 138 classmates with names, emails and
  9-digit `numeroDossier`. Therefore: no PII in `Debug`/`Display` output, no
  on-disk response cache by default, and raw bodies in error paths only behind
  an opt-in env var.
- **Be a polite client.** Default to bounded concurrency and no tight retry
  loops. This is a university's production system.

---

## 2. Chunks

Each chunk is independently buildable, testable and committable. Dependencies are
listed; anything with the same dependencies can be done in any order.

### C0 — Dependency and template cleanup
**Depends on:** nothing · **Size:** XS

- Remove `jwt 0.16` — Brio's token is an opaque UUID, there is nothing to parse.
- Remove the `add()` cargo-template function and its test.
- Declare real deps: `serde` (derive), `serde_json`, `chrono` (serde), `thiserror`,
  `reqwest` (json, cookies), `tokio`, `url`.
- Add `rust-toolchain.toml` pinning the edition-2024-capable toolchain.

**Done when:** `cargo build` and `cargo clippy` are clean with no unused deps.

---

### C1 — HAR fixture extraction and sanitization
**Depends on:** C0 · **Size:** M · *Prerequisite for all type work*

A script (Python, in `tools/`) that walks the HAR and writes one JSON file per
distinct endpoint to `tests/fixtures/<service>/<operation>.json`, with a manifest
recording the request URL, query params and status for each.

Redact, preserving shape and length class: `prenom`, `nom`, `nomAffichage`,
`pseudonyme`, `courriel`, `courrielPrincipal`, `numeroDossier`, `nie`,
`identifiant`, `identifiantConnexion`, `name`, `preferred_username`, the avatar
base64 blob, every `access_token`/`jeton`/`Signature`/`session_state`, and all
cookie values.

**Done when:** fixtures are committed, contain no PII, and a grep for the
redaction patterns comes back clean. Verify by re-running the credential scan
used before the first commit.

---

### C2 — Core primitives
**Depends on:** C1 · **Size:** M

- `Id<T>` — opaque newtype over `String` with `PhantomData<T>`; `Display`,
  `Serialize`, `Deserialize`, `FromStr`, `From<String>`, manual `Clone`/`PartialEq`
  to avoid spurious `T` bounds. Marker types per resource. No validation.
  Helper: `split_composite() -> Option<(&str, &str)>` — opt-in, because
  `idEvaluation` is composite in 16 samples and plain in 9.
- Envelopes: `Element<T>` (`{element}`), `Elements<T>` (`{elements}`),
  `Page<T>` (`position`, `taille_page`, `nb_resultats_courant`,
  `nb_resultats_total`, `position_page_suivante`, `position_maximale`, `elements`).
- Shared types: `Periode`, `SuiviModifications`, `Autorisations`, `TexteRiche`,
  `RefFichier`, `Note`.
- `wire_enum!` macro generating a `Connu`/`Inconnu(String)` pair per D12.
- Datetime aliases: `Horodate = DateTime<FixedOffset>`, `HorodateLocale = NaiveDateTime`,
  `Heure = NaiveTime`.

**Done when:** every shared type round-trips at least one fixture, and a test
asserts an unknown enum value deserializes to `Inconnu` rather than failing.

---

### C3 — Error model
**Depends on:** C2 · **Size:** S

```rust
pub enum Error {
    Unauthorized,
    ReauthRequired,
    Api { status: StatusCode, erreurs: Vec<ErreurValidation>, raw: String },
    Decode { endpoint: &'static str, source: serde_json::Error },
    Transport(reqwest::Error),
}
```

`ErreurValidation` mirrors the partner spec (`codeErreur`, `message`, `propriete`,
`valeur`) and tolerates the plural `{"erreursValidation":[…]}` wrapper its prose
implies. **The app API's error shape is unverified** — the HAR contains zero 4xx/5xx
— so parsing must fall back to retaining the raw body rather than failing.

`Decode` names the endpoint, because inferred-from-one-sample types will be wrong
and the log has to say which. Raw bodies gated behind `BRIO_DEBUG_BODIES`.

**Done when:** an unparseable error body yields `Api` with `raw` populated, not a panic.

---

### C4 — Session and authentication
**Depends on:** C3 · **Size:** L · *Highest-risk chunk*

- `Session { cookies, access_token, expires_at }`, serialized to
  `$XDG_STATE_HOME/brio-rs/session.json` at 0600 (enforced, and verified on load).
- Cookie-jar import from a pasted cookie header or a browser-exported JSON.
  Required cookies: `session`, `session-app-pr`; carry `client`, `clients` too.
- `refresh()`: `GET https://identites.brioeducation.ca/auth/oauth2/authorize/`
  with `response_type=token`, `client_id=ena2-ui`,
  `redirect_uri=https://www.brioeducation.ca/auth/response`, fresh `nonce`,
  base64-JSON `state`. Do **not** follow the final redirect — parse
  `access_token`, `expires_in`, `state`, `nonce` out of the `Location` fragment.
  Verify the echoed `nonce` matches.
- Distinguish success from session death: a `Location` pointing at `/auth/`
  instead of carrying a fragment means `ReauthRequired`.
- Proactive refresh timer plus the `< 60s` pre-flight check (D7).

**Done when:** fragment parsing, nonce verification and the `ReauthRequired`
branch are unit-tested against the real redirect strings from the HAR. Live
refresh needs a manual smoke test with an imported jar.

---

### C5 — HTTP core
**Depends on:** C4 · **Size:** M

- `Brio` holding a `reqwest::Client`, the `Session`, and per-service base URLs
  (`apis.brioeducation.ca/{sites,formations,communications,forums,lti,sitepromo}/`,
  plus `identites.` and `fichiers.` hosts).
- Bearer injection; `Accept-Language: fr-CA` (responses are localized).
- Request-builder infrastructure, including **repeated-key** query serialization —
  `inclurerelations` is sent as repeated keys, not comma-joined.
- Pagination: `.send()` → `Page<T>`; `.all()` loops until
  `position_page_suivante == -1`, with a hard iteration cap as a runaway guard.
- `brio.raw()` escape hatch for the four unexercised services and anything the
  HAR missed.
- Bounded concurrency default.

**Done when:** a fixture-backed mock server exercises paging to exhaustion and
repeated-key encoding.

---

### C6 — `identites`: connect and userinfo
**Depends on:** C5 · **Size:** S · *First end-to-end path*

- `GET /auth/oauth2/userinfo/` → `Utilisateur`, `Identite`, `Compte`.
- `Brio::connect()` caches `id_utilisateur` and `ids_unites_organisations_liees`
  so D6's defaults work.
- `GET /unites/:id` and `/unites/:id/parametres/:code` (note: `idParametreUnite`
  is `<uniteId>-<CODE>`, a third id shape).

**Done when:** `connect()` against a fixture mock yields the cached identity.

---

### C7 — `sites`: the course list
**Depends on:** C6 · **Size:** L · *First useful output*

- `GET /sites/sommairessites/` (paged), `/sommairessites/:id/`, `/sites/sites/:id/`,
  `/sites/sommairessitescredites`.
- The largest type in the crate: `SommaireSite` with nested `GroupeApprenants`,
  `InformationCours`, `InfosApprenant`, `Fonctionnalite` (21 observed values →
  `wire_enum!`).
- `examples/liste_cours.rs` printing course codes and names.

**Done when:** the example lists all 13 sites from the fixture, and `cargo run`
does the same against a live session.

---

### C8 — `sites`: timetable and sessions
**Depends on:** C7 · **Size:** S

- `GET /sites/sites/:id/plageshoraires` → `jour`, `heure_debut`, `heure_fin`,
  `local`, `adresse_complete`, `type` (`CLAS`/`ATEL`).
- `GET /sites/sessions`.

**Done when:** `Heure` fields parse and a fixture test covers both plage types.

---

### C9 — `sites`: evaluations, results, grade scale
**Depends on:** C7 · **Size:** M

- `/evaluations/`, `/evaluations/agregationevaluations`, `/evaluations:autorisations`
- `/resultats/:id` → `note_cumulee`, `ponderation_cumulee`, per-evaluation abstracts
- `/baremesnotation`, `/baremesnotation:rechercher`, `/sites/sites/cotes`
  (12-value `Cote` enum)
- `/conditionsreussite`

**Done when:** a fixture test reconstructs the weighted total from
`agregationevaluations` and matches `total_ponderation`.

---

### C10 — `sites`: menus, pages, content
**Depends on:** C7 · **Size:** XL · *Largest type surface*

- `/sites/menus/:cid/` — recursive `elementsMenu`, `typePage` enum
- `/sites/pages/:cid/?inclurecontenus=true` — `zones[].elements[]` plus the
  `elementsLies` side-load, keyed type-name → id → object
- `/sites/modulesapprentissage/:cid`,
  `/sites/listeselementlistemodulesapprentissage/:cid/`
- `Contenu` as an internally-tagged enum on `type`, with the 7 observed variants
  (`sites.MotBienvenue`, `ContenuTexte`, `ListeFichiers`, `ListeModulesApprentissage`,
  `ContenuSectionPage`, `ProductionWeb`, `ListeEvaluationsSommatives`) and an
  `Inconnu` variant retaining raw JSON.

**Done when:** all 50 `pages` fixtures deserialize, and an injected unknown
`typeContenu` lands in `Inconnu` without error.

Consider splitting into C10a (menus/pages skeleton) and C10b (`elementsLies`
content variants) if it runs long.

---

### C11 — `formations`
**Depends on:** C6 · **Size:** M · *Parallel with C7–C10*

- `/formations/unites`, `/sessionsformationscreditees`
- `/apprenants/:id/formations:agregation`, `/formateurs/:id/formations:agregation`
- `/utilisateurs/:id/rechercherformations`, `:rechercherformationscreditees`

---

### C12 — `communications`
**Depends on:** C6 · **Size:** S · *Parallel*

- `/notifications` (filters `etat=NON_LU`, `taillepage=-1`)
- `/notifications/-/utilisateurs/:id/etatconsultation`
- `/messagessystemelocalises?statut=EN_COURS`

This is what the daemon polls (D13).

---

### C13 — `forums`
**Depends on:** C6 · **Size:** M · *Parallel*

- `forums:obtenirparidentifiantexterne` (keyed by `idSite`)
- `/sommairesforums/:id`, `/forums/-/canals/:id`, `/canals/:id/discussions`

---

### C14 — `fichiers` and learner lists
**Depends on:** C7 · **Size:** M

- Two-hop download: `GET /sites/sites/:id:jetonlti` (and the file-token
  equivalent) returns a bare `text/plain` `v2~A~B` token, fed as `?jeton=` to
  `GET /fichiers/:id/contenu`, which 302s to a signed CloudFront URL. Follow
  deliberately; do not leak the bearer token to CloudFront.
- `/sites/sites/:id/plancours` (course-plan PDF refs)
- `/sites/sites/:id/apprenants` and `/utilisateurs` — **PII-bearing**; these are
  the types that must stay out of `Debug`.

---

### C15 — Polish
**Depends on:** all · **Size:** M

- Crate-level docs with an auth-setup walkthrough
- README: what this is, that it is unofficial, how to import cookies
- `examples/` for the dashboard queries
- Decide crates.io publication; if yes, rename `brio-rs` → `brio`
  (the `-rs` suffix is discouraged) and add license files

---

## 3. Suggested order

```
C0 → C1 → C2 → C3 → C4 → C5 → C6 → C7 ──┬→ C8
                                         ├→ C9
                                         ├→ C10 (C10a, C10b)
                                         └→ C14
                              C6 ────────┬→ C11
                                         ├→ C12
                                         └→ C13
                                                  → C15
```

C0–C7 is the critical path to something useful: authenticate, then list your
courses. Everything after C7 is additive and parallelizable.

## 4. Open risks

1. **Server-side session lifetime is unknown.** The cookies claim 10 years, but
   HAR entry #0 shows a previous session had already died server-side while
   `client`/`clients` persisted — and the login page ships an idle-timeout modal.
   How often a human must re-import is unmeasured. Measure it in C4 by logging
   refresh failures.
2. **Optionality is guessed.** One capture cannot prove a field is always
   present. Default to `Option<T>` + `#[serde(default)]`; expect `Decode` errors
   in early daemon runs and treat each as a fixture to add.
3. **The error shape is unverified.** Zero non-2xx responses were captured.
4. **`idEvaluation` is shape-inconsistent**, so composite parsing can never be
   assumed from a field name alone.
5. **Brio ships continuously.** D12 mitigates enum drift, but added *required*
   fields or restructured objects will still break. Fixtures detect drift only
   when refreshed from a new capture.
