# OTP Layering

From *Designing Elixir Systems with OTP* — shape data and APIs at each boundary.

## Core layer

- Pure functions on domain types.
- No side effects, no clock, no random — inject via arguments when needed.
- Returns `{:ok, _}` / `{:error, _}` or domain-specific results.
- Example: `MyApp.Billing.calculate_total/2`

## Boundary layer

- Validates and sanitizes external input (HTTP params, JSON, CLI args).
- Converts to core types; converts core errors to boundary-friendly messages.
- Example: `MyApp.BillingBoundary.parse_invoice_params/1`

## Service layer

- Coordinates core + multiple boundaries (Repo, HTTP client, email).
- May own a GenServer when serialization or caching is required.
- Example: `MyApp.Billing.create_invoice/2`

## API / delivery layer

- Phoenix controllers, LiveViews, plugs, channels.
- Thin: parse connection/assigns, call service/context, render response.
- No `Repo` calls or business rules in controllers/LiveView event handlers.

## Data flow

```
Request → Plug → Context/Service → Core
                ↓
              Boundary (Ecto changeset, external API)
                ↓
              Core (pure)
```

## API design at boundaries

- Public functions take **minimal** typed arguments, not entire conn structs.
- Return tagged tuples or raise only at true programmer-error boundaries.
- Keep arity low; use opts keyword or struct for optional params.

## Files and modules

Typical Phoenix app mapping (names vary):

| Layer | Location |
|-------|----------|
| Core | `lib/my_app/domain/` or inside context as private pure fns |
| Context | `lib/my_app/accounts.ex` |
| Schema / Boundary | `lib/my_app/accounts/user.ex` |
| Web | `lib/my_app_web/` |

Contexts are the service+boundary bundle Phoenix promotes — keep core logic extractable and testable without `MyApp.Repo` in every test when possible.
