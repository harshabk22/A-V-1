## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, run `./scripts/graphify-status` and read `graphify-out/GRAPH_REPORT.md` for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- After modifying code files in this session, run `./scripts/graphify-refresh` to keep the graph current (AST-only, no API cost)

## Superpowers

This project uses the Superpowers (obra/superpowers) agentic skills framework for
disciplined development. See https://github.com/obra/superpowers.

The coding agent MUST follow the Superpowers workflow:

1. **brainstorming** — Before writing any code, refine requirements through
   questions, explore alternatives, present design in sections for validation.
   Save the design document.

2. **using-git-worktrees** — After design approval, create an isolated workspace
   on a new branch, run project setup, verify clean test baseline.

3. **writing-plans** — Break work into bite-sized tasks (2-5 minutes each). Every
   task has exact file paths, complete code, and verification steps.

4. **test-driven-development** — RED-GREEN-REFACTOR: write failing test first,
   watch it fail, write minimal code, watch it pass, commit. Delete any code
   written before tests.

5. **subagent-driven-development** — Dispatch fresh subagent per task with
   two-stage review (spec compliance, then code quality).

6. **requesting-code-review** — Review against plan between tasks. Critical
   issues block progress.

7. **finishing-a-development-branch** — When tasks complete, verify tests,
   present merge/PR options, clean up worktree.

The agent checks for relevant skills before any task. These workflows are
mandatory, not suggestions.
