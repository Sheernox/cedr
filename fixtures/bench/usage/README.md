# Reduced vendor usage fixtures

These contain usage counters only. Prompts, workspace paths, session ids, messages and sign-in material are excluded.

- Claude: the real headless result already captured by Compare's adapter tests, reduced to usage fields. `claude-models.json` is a real SDK initialize capability response, reduced to the Haiku model row.
- Codex, Muse and Grok: real headless tool probes captured in isolated containers with the installed versions during Bench implementation.
- Antigravity: real local ACP tool probe, normalized from its conversation database by Monitor's counts-only protobuf collector.
- Gemini: the official [headless JSON example](https://geminicli.com/docs/cli/headless/), reduced to one model's token counters. This adapter remains unverified.
- Command: the documented custom-program protocol example.
