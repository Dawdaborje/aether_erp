# Alerts (`erp_alerts`)

## Purpose

A morning digest of what needs attention across the ERP, so people stop having to open six dashboards. App route `/erp/alerts`.

## Dependencies

`erp_inventory`, `erp_manufacturing`, `erp_procurement`, `erp_quality`, `erp_replenishment`, `company`

## Models

| Model | Role |
|---|---|
| `alert_subscription` | One per person (`user`, unique): optional `company`, `active`, `expiry_days`, a switch per check, `last_sent` |

## Design

- **Schedule:** `daily-digest` (`0 7 * * *`, UTC) runs `sweep`, which queues one `digest` job per active subscriber with `unique_key = digest-<id>-<date>`, so running the sweep twice a day queues nothing twice.
- **Runs as the subscriber:** the job uses `scheduler::run_as`, so every `report::query` follows that person's roles and record rules. A check they may not read is left out silently; the digest never shows more than they could open. `run_as` takes the user from the subscription record, never from input.
- **Checks** (counts, from `report::query`): lots expiring within `expiry_days`; manufacturing orders past `planned_date` and not done; approved supplier bills past `due_date` (with their total); open replenishment requests; inspections rejected in the last 7 days.
- **Delivery:** one `notify::send` to the person, only when at least one line exists.
- **Roles:** `alerts_user` manages their own subscription, `alerts_admin` lists all and can queue the digests now.

## Functions

`subscribe`, `my_subscription`, `list_subscriptions`, `sweep`, `digest`, `send_now`

## Boundaries

- The schedule is in UTC; a per-organization time zone is not set.
- Notification only: no email or SMS yet (`communication::send` would add it).
- A user who loses their account or role gets a failed job for good, not a retry (kernel behaviour for `run_as`).
- Low stock is covered through open replenishment requests, not recomputed here.
- `list_subscriptions` returns every subscription to any subscriber unless the model's record rules narrow it; they currently do not.
