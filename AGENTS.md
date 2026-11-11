# AGENTS.md

Instructions for AI agents (and humans) working in this repository.

vericount is an application to keep the books of a Spanish SL and comply with Veri*factu. The current plan is the most recent roadmap in [`ROADMAP/`](ROADMAP/).

## Language

All repository content must be in English: code, identifiers, comments, documentation, roadmaps, records, file names and commit messages. This applies even when the conversation with the author is in another language.

Allowed exceptions:

- Official names of Spanish laws, bodies and legal concepts (e.g. Veri*factu, AEAT, NIF, Orden HAC/1177/2024). Add a short English explanation where it helps.
- Identifiers and data defined by external specifications, such as AEAT WSDL/XSD element names and the fixtures that mirror them.
- The texts shown to users. The interface is in Spanish, and all of its texts live in `web/src/i18n/es.ts`. Keys, identifiers and comments stay in English, and so do the API's error codes and messages; the client maps error codes to Spanish texts. The emails the server sends are in Spanish too, and their texts live in `crates/server/src/mail/es.rs`; so are the messages of the installer, `setup.sh`.

## Roadmaps: `ROADMAP/YYYY-MM-DD/`

- Each roadmap lives in its own directory named after its date (`YYYY-MM-DD`).
- The main file is `ROADMAP/YYYY-MM-DD/README.md`. It must include a Gantt chart in a fenced `mermaid` code block.
- Supporting files (more `.md` files, diagrams, data, etc.) may sit next to it and should be linked from the main file.
- The most recent date is the current roadmap. To revise the plan on a later day, create a new dated directory instead of rewriting an older one.

## Memory: `RECORD/`

`RECORD/` is the project's memory across sessions and agents.

File name: `RECORD/YYYY-MM-DD.<summary>.(WIP|completed).md`

- `YYYY-MM-DD`: the date the record was started.
- `<summary>`: a short kebab-case description in English (e.g. `aeat-mock-setup`).
- `WIP` or `completed`: the status of the record.

Rules:

- `*.WIP.md` files are **append-only**: add new content at the end; never edit, reorder or delete existing content.
- `*.completed.md` files are **read-only**: never modify, rename or delete them.
- To close a record, append a final entry, then rename it from `.WIP.md` to `.completed.md` (`git mv`) without changing its content.
- To follow up on completed work, start a new record and link to the previous one.

Workflow:

1. Before starting a task, read the open `*.WIP.md` records and any `*.completed.md` records related to it.
2. While working, append decisions, progress, open questions and next steps to the matching WIP record, or create a new one.
3. Start each appended entry with a heading that includes the date, e.g. `## 2026-11-11: Short title`.

## Code organisation

Code is organised by domain: access, company, contacts, invoices and aeat, with the same folders in `crates/server/src` and `web/src` (see the Layout section of the README). A domain folder holds its data access, its rules, its HTTP routes (`api.rs`) and, in the client, its types, endpoints and screens. What every domain uses lives outside them (`api.rs`, `error.rs`, `events.rs`, `config.rs` on the server; `app/`, `i18n/`, `theme/` in the client). New code goes in the folder of its domain; code that two domains need goes in the one that owns the data, and the other imports it.

## Frontend

The web client (`web/`) is written with jq79 and TypeScript:

- Components are jq79 single-file `.html` components with `<script :setup lang="ts">`.
- Everything else is `.ts`; no plain JavaScript sources.
- No user-facing text is written in components or modules: it goes in `web/src/i18n/es.ts`.
- Components are precompiled with the jq79 Vite plugin (`safeEval: true`), so the client runs under the strict CSP without `unsafe-eval`.
- `npm run build` must pass, including the type check. The jq79 plugin strips the types of component scripts but does not check them; `tsc` only checks the `.ts` files.
