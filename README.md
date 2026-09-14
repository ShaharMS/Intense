# Intense

Intense is my project playground.

I use it to explore focused projects under codenames: different ways to pursue a goal, different ways to test an idea, and different pieces that may eventually belong to a coherent system. A codename is a project, but it is also an experiment in how that project can be approached.

The point is not to decide too early that there is only one correct implementation. Two projects may have the exact same goal and still deserve to exist independently because they solve it in different ways. They can be compared on evidence, kept for different circumstances, or combined when their boundaries and behavior make that useful.

## How to think about the playground

The basic structure is:

```text
goal/
└── codename/
```

- A **goal** is the problem, capability, or outcome being explored.
- A **codename** is an independent project pursuing that goal through a particular approach.
- A project may contain any focused artifact that helps test the idea: code, a prototype, research, benchmarks, notes, or an integration experiment.

Projects under the same goal should be easy to compare without losing their individual identity. A project can also become useful alongside another project as part of a larger system. The playground recognizes both possibilities, but it does not maintain a composition or compatibility ledger yet. Combinations can remain implicit until there is a reason to document them deliberately.

## Project lifecycle

Every codename uses one of four simple statuses:

- **Idea** — the project has been proposed but not meaningfully explored yet.
- **Active** — the project is currently being built, tested, or investigated.
- **Validated** — the project’s stated goal is supported by its own documented evidence.
- **Archived** — the project is no longer active, including projects that were rejected or superseded.

Validated does not mean production-ready, universally best, or automatically selected as the default approach. It only means that the project met the success criteria it defined and recorded.

## Goals and comparison

When several codenames pursue the same goal, the goal may define shared comparison criteria where that makes the comparison fair or useful. Each project still owns its own success criteria and evidence.

The comparison should answer practical questions:

- What approach does each codename take?
- What does the evidence support?
- Where does each approach work well or fall short?
- Is one approach preferable for a particular context, or are several worth keeping?

An unsuccessful approach is still part of the playground’s history. Archive it with a short explanation of what was learned and why it was rejected or superseded.

## Project catalog

### streaming

- [River](streaming/river/) — **Active** — browser-first, raster-only streaming of selected JPEG 2000 regions with measurable progressive delivery.

When the first project exists, list it under the relevant goal with a link to its project directory and a concise project card. Keep this catalog focused on orientation; the project itself should hold deeper implementation and experiment details.

## Project brief

Copy this brief when creating a new codename:

```markdown
# <Codename>

## Goal

<What problem, capability, or outcome is this project exploring?>

## Approach

<What makes this approach distinct from other projects pursuing the same goal?>

## Status

Idea

## Success criteria

- <What must be true for this project to meet its goal?>
- <Add project-specific criteria rather than assuming another project’s criteria.>

## Evidence

- <Link or describe the test, measurement, observation, or result supporting the current status.>

## Next step

<What is the next concrete experiment or decision?>

## Archive or decision note

<Complete this section when the project is archived, rejected, or superseded. Explain what was learned and why.>
```

The brief is intentionally small. It should make the project’s intent, approach, maturity, evidence, and next move clear without turning every experiment into a formal process.
