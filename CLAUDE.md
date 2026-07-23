# CLAUDE.md

Project guidance for Claude Code. See [CONTEXT.md](CONTEXT.md) for the domain
vocabulary, [docs/architecture.md](docs/architecture.md) for the rules, and
[docs/code-map.md](docs/code-map.md) for a guided code tour.

## Mermaid diagrams

The repo carries Mermaid guidance under `.github/` that was authored for GitHub
Copilot in VS Code (`copilot-instructions.md` → `.github/instructions/mermaid.instructions.md`).
Claude Code cannot use most of that machinery — treat it as follows.

**Does NOT apply to Claude (VS Code / Copilot extension only), ignore it:**
- The LM tools `mermaid-diagram-validator`, `mermaid-diagram-preview`,
  `get-syntax-docs-mermaid` — Claude has no access to these.
- VS Code command IDs (`mermaidChart.preview`, `mermaidChart.createMermaidFile`, …).
- `@mermaid-chart` slash commands and the Mermaid Chart Sync/AI-credits workflow.

**DOES apply — portable rules Claude should follow when producing a diagram:**
1. Pick the correct diagram type and emit valid Mermaid: correct first-line
   keyword (`graph TD`, `sequenceDiagram`, `stateDiagram-v2`, `erDiagram`, …),
   valid arrow types, balanced brackets/quotes.
2. Keep diagram text ASCII — no stray non-ASCII characters inside nodes/labels
   (they break rendering).
3. In Markdown docs, use fenced ```mermaid blocks (this repo already does —
   see docs/architecture.md and docs/code-map.md). A standalone `.mmd` file is
   only worth it when the diagram is meant to be previewed by the VS Code
   extension; for docs, inline fenced blocks are preferred.
4. Do not hand-edit any diagram marked as managed by the Mermaid Chart Sync app
   (it carries a `id:` frontmatter block) — regenerating those is the
   extension's job, not Claude's.

Net: Claude writes plain, valid, inline Mermaid and leaves the VS-Code-specific
tooling to Copilot users.
