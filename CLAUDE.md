## Trurl

This project uses Trurl for architectural decisions. The Trurl MCP server enforces design-before-implementation.

### Workflow

Before implementing any task:

1. Call `advance` with the component name and task_type. (Omit task_type to let Trurl infer from graph state.)
2. Follow the returned `action` exactly.
3. If the action says `get_step_prompt` — call it, follow the instructions, then call advance` again.
4. Repeat until `ready: true`.
5. Implement, constrained by every decision in `get_context`.

### Task Types

| Type | When to use |
|------|-------------|
| `new_component` | Building something that doesn't exist yet |
| `feature` | Adding to an existing component |
| `fix` | Bug fix or hotfix |
| `learn` | Understanding existing architecture |
| `review` | Challenging decisions for drift |
| `harden` | Filling coverage gaps |

### Comprehension Gates

When Trurl's step prompt includes comprehension checkpoints, ask the user to articulate their understanding in their own words. The user explains — you validate. Do not explain on their behalf.

### Decision Recording

When `record_decision` returns a `pattern_opportunity` field, present it to the user. If confirmed, call `record_pattern` immediately with the listed decision names. Do not defer.

### Scope Rules

- **Project scope**: cross-cutting principles (error strategy, coding standards, security posture, dependency policy, build configuration).
- **Component scope**: decisions specific to that component's implementation. If a decision applies to multiple components equally, it is a project rule.
