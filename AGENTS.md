# Playscale development guidance

## State and correctness boundaries

For changes to application behavior, explicitly consider whether the logic is a
state-dependent correctness rule. If correctness depends on prior events,
identity, revisions, ownership, ordering, cancellation, retries, recovery, or
publication, prefer representing the relevant facts and decisions in deterministic
state and transitions in `crates/core`.

- Model the facts needed to decide whether an action is valid. Do not assume that
  a job's phase and attempt counter capture every relevant condition.
- Keep correctness decisions in the production core wherever practical. Adapters
  should supply observations and execute effects. Avoid hiding domain decisions
  behind a generic `success` boolean when the model needs the underlying facts
  to verify the rule.
- For example, accepting a processing result may depend on source revision,
  attempt identity, output validation, cancellation, and prior publication.
  Consider these explicitly when changing that behavior.
- Keep SQLite, filesystem access, subprocesses, network calls, and timers in
  adapters. Represent their relevant observations, failures, and acknowledgments
  as explicit inputs when they affect correctness. Express required atomicity
  and durable ordering at the core/adapter boundary and verify their enforcement
  with integration tests.
- Use focused domain models and pure policies. Do not force all application data,
  ordinary reads, media bytes, or incidental implementation details into a global
  state machine. Do not duplicate state that can be derived reliably.

## Stateless verification

Stateless should exercise the same decisions the application executes, rather
than a separate implementation of the intended behavior.

- When adding or changing a state-dependent rule, consider extending the model's
  state, inputs, and properties alongside the production core. The current job
  model is a starting point, not a ceiling on what Stateless can represent.
- Assert intended outcomes and effect identities/counts where relevant, not just
  the presence of an effect or the absence of a crash. Consider stale and duplicate
  events, cancellation, retries, recovery, and meaningful boundary values.
- Model relevant concurrency through explicit event interleavings and crash or
  failure inputs where useful. Continue testing real transaction, filesystem,
  and worker behavior separately; model checks do not prove adapter conformance.
- Report explored bounds, checked properties, and remaining gaps accurately.
  Distinguish a missing model or property from a demonstrated library limitation.

For correctness-sensitive changes, briefly explain in the change description
which rules belong in core state/transitions, what verification covers them, and
why any relevant decisions remain in adapters. A reasoned boundary decision is
required; an unrelated wholesale refactor is not.
