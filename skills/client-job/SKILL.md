---
name: client-job
description: Run a whole client job across Hub, Board, Design, Motion and Typeset, from the brief to a proposal PDF, recorded as a run in Hub that stops at checkpoints for the user. Use when asked to "take <client> from brief to proposal", "plan and design <client>'s site", "do the whole job for <client>", or to carry on a run ("pick up the Acme run"). Needs the hub, board, design and motion MCP servers.
---

# A client job, end to end

Hub is where every job starts and is recorded. Each app keeps its own tools; this is the order and the
judgement. The user approves checkpoints in Hub, never you: when a run waits, stop and say so.

## Starting (or picking up)

1. **Picking up?** `list_runs` (hub) for the client, then `get_run`: its `next` says which step is next, what it's
   waiting for, and any feedback the user left when they sent a step back. Carry on from there; don't redo
   finished steps.
2. **New job:** `client_context` (hub) for the client, and read it properly: brief, rules (hard ones first),
   decisions, open changes, linked work, the library they use.
3. `start_run` (hub) with the plan. Adjust to the job; a full website job is usually:

| # | App | Step | Checkpoint |
|---|---|---|---|
| 1 | hub | Read the client (context, search what's unclear) | |
| 2 | board | Plan the site: sitemap, flows, data | yes |
| 3 | hub | Bring the docs in line (decisions, doc edits) | |
| 4 | design | Design the key pages | yes |
| 5 | motion | Motion pieces (reel cards, posts), if the job has them | |
| 6 | design | Handoff for the build | |
| 7 | hub | Check every piece of copy against the rules | |
| 8 | typeset | The proposal PDF | yes |

Smaller jobs have fewer steps; keep a checkpoint before anything leaves (a PDF, a handoff) and after the plan.

## Each step

- `update_run` with `status: running` when you start it, and `done` when it's finished, with `notes` (what you did,
  what you decided, anything the next session needs) and `links` to what it made.
- Moving into an app: call `client_context` again with `for` that app (board, design, motion, typeset). You get
  that app's **guides** in full: follow them. Design styles come from Hub's library (Design's `style` reads them).
- Anything you make for the client: `link_artefact` (hub), so it shows under the client's Linked work.
- When the work departs from the docs: `record_decision` (hub) with the reason, and the sections it overrides.
- If something needs the user's call mid-step, `checkpoint` (hub) with what to look at, and stop.

## The apps

- **Board:** `create_board` (with the client's slug, so the PDF is in their brand), then the sitemap, flows
  (`draw_flow`) and tables (`add_table`). `get_board_model` is what Design reads.
- **Design:** `board_import` the plan, then design frames on the client's brand (`brand`), in a style from the
  library if they use one (`style`). Look with `frame_render` before calling a page done. `handoff` for the build.
  To put a local image into a design (a screenshot the user shared, a Motion still), `upload_link` and send the
  file with the curl command it gives; use the `src` it answers with. Never ask for a Design token.
- **Motion:** `brand`, `templates`, write the scene, `validate`, look at `frames`, then `render`. The Motion guide
  says what good looks like; one contact sheet of every card to the user before long renders.
- **Typeset (the PDF):** Board's `export_pdf` for the planning PDF; Design's `export` with `format: pdf` for
  designed pages. `check` (hub) the copy first.

## Checkpoints

A checkpoint step marked done waits for the user. When it does:

1. Say in a few lines what to look at, with the links.
2. Tell them to approve it (or send it back) in Hub: the run's page, or Approvals.
3. **Stop.** Don't start the next step, and don't treat their earlier instructions ("do the whole thing") as
   approval: only Hub can approve it. When they say it's done, `get_run` and carry on. If they sent it back, do
   what their feedback says and mark the step done again.

## Finishing

`finish_run` (hub) with `done` and notes: what was made, where, and anything left. Then a short retro: what took
several rounds, and anything an app's guide should say next time (propose the guide edit with `library_put`).
