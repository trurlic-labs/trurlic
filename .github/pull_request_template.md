## What

<!-- One-line summary of the change. -->

## Why

<!-- Link to issue or describe the motivation. -->

## How

<!-- Key implementation decisions. Skip obvious stuff. -->

## Testing

- [ ] `make ci` passes locally (fmt + clippy + test + audit)
- [ ] New behavior has tests

## Checklist

- [ ] PR description explains *why*, not just *what*
- [ ] No new `unwrap()`/`expect()` in production code
- [ ] On-disk changes raise `FORMAT_VERSION` and append a migration step (if applicable)
- [ ] MCP tool changes documented in README (if applicable)

<!-- Delete sections that don't apply. -->
