# Quality (`erp_quality`)

## Purpose

Inspect goods against a checklist. A **template** lists parameters (a measurement that must lie in a
range, or pass / fail); an **inspection** snapshots those parameters as **readings** for a document
(a receipt, a delivery, a manufacturing order) and ends accepted or rejected.

## Dependencies

`erp_item` (items), `company`.

## Models

| Model | Role |
|---|---|
| `quality_template` | `code`, `name`, optional `item`, `applies_to` (`any` / `receipt` / `delivery` / `production`), `active`; `parameters` is a child field |
| `quality_parameter` | `name`, `kind` (`numeric` / `pass_fail`), `min` / `max` |
| `quality_inspection` | Numbered `QI-{YYYY}-{#####}`; template, item, company, `source_kind` / `source_id`, `lot`, `qty`, `state`, `inspected_on`; `readings` child field |
| `quality_reading` | Snapshot of a parameter plus the `value` and `result` (`pending` / `pass` / `fail`) |

States: `draft` → `accepted` / `rejected` / `cancelled`; an accepted or rejected inspection can be reopened by `quality_admin`.

## Design

- **Kernel does the bookkeeping:** the number is a naming series, parameters and readings are `child` fields written with their parent, `reading_count` is a computed `count(readings)`, chatter tracks the state, a model `check` refuses a maximum below the minimum, and `unique_when` allows one active template per item and stage.
- **The workflow is data:** `rules/quality_inspection.json` says which roles may accept, reject, cancel and reopen; the handlers are plain state writes and a refused move changes nothing.
- **Readings are snapshots:** editing a template later never changes an inspection already started.
- **Deciding:** `record_reading` sets a numeric reading's result from its range (inclusive) or takes pass / fail; `complete_inspection` needs every reading recorded and rejects if any failed.
- **`inspection_status(source_kind, source_id)`** answers `none`, `pending`, `accepted` or `rejected` (cancelled inspections do not count, a rejection wins). Callers use it to hold a release; no plugin is forced to.

## Functions

`create_template`, `create_inspection`, `record_reading`, `complete_inspection`, `cancel_inspection`, `reopen_inspection`, `inspection_status`, `list_inspections`, `list_templates`, `list_readings`

## Roles

`quality_reader`, `quality_user` (create inspections, record readings, decide), `quality_admin` (templates, reopen).

## Boundaries

- Stock is not blocked by this plugin: the receipt and delivery gates live in `erp_procurement` and `erp_selling`, which list `erp_quality` under `optional_dependencies` and ask `inspection_status` only when it is installed and the company turned `require_inspection` on.
- No sampling plans, no AQL tables, no non-conformance or corrective-action records.
