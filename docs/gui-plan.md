# GUI improvement plan

## Direction

Make the topology the main workspace, using the layout conventions of Adobe
tools and Google Slides: compact navigation, a large persistent canvas, and
panels that open when needed. Use a restrained dark palette and short, factual
labels.

The revised request adds five requirements to the original work:

- The canvas dominates the normal view, as well as supporting fullscreen.
- Resource summaries expand from totals to labs to components and containers.
- MCP tools and recorded actions have human-readable display names.
- Theme selection follows the operating system and supports manual overrides.
- Show the lab's highest observed block height, updated live.

## 1. Canvas and application layout

- Use a viewport-height application shell with a compact top toolbar and a
  collapsible lab navigator on the left.
- Put the lab name, readiness, and essential controls in the toolbar. Remove
  the large heading and metric-card row above the topology.
- Show a compact “Block 1,234” indicator in the canvas toolbar, visible in
  both the normal and fullscreen views.
- Fill the remaining central area with the topology; avoid document-style page
  scrolling in the lab view.
- Open a collapsible inspector on component selection. Keep it closed by
  default so it does not consume canvas space.
- Move Activity and Sessions into a collapsible bottom drawer with tabs.
- Provide pan, zoom, fit-to-lab, fullscreen, and Escape to exit fullscreen.
- Preserve selection, pan, zoom, drawer state, and fullscreen across live
  updates. Reset or restore the viewport deliberately when changing labs.
- On smaller screens, use overlay panels and keep the canvas usable.

## 2. System summary and resource view

- Keep a compact, clickable System summary in navigation: CPU, memory, and
  running container count.
- Open a dedicated System view with workspace totals and expandable rows:
  lab → component → container. Put shared probes and action jobs in explicit
  groups so every measured container contributes exactly once.
- Include CPU usage, memory usage, running/ready counts, restart counts, and
  process state. Show requests and limits as separate fields from usage.
- Treat “processes” as Kubernetes workload containers. Scope totals to the
  labs tracked in the current database, workspace, and selected cluster;
  identify that scope in the view.
- Allow filtering by lab, component, process name, and running/stopped state.
- Link a resource row back to its component on the topology.
- Show sample times, partial coverage, and unavailable measurements accurately.
  Missing samples must not become zero usage.
- Keep storage reservations distinct from actual disk consumption; show disk
  usage only when a measured source is available.

## 3. Component tiles and live values

- Sample current block heights from the lab's Bitcoin nodes through passive
  reads and display their maximum as the lab block height. Use validated block
  height, not header count or a lifetime high-water mark; the value can decrease
  after a reorganization or reset.
- Refresh the lab block height through SSE alongside balances. Show individual
  node heights in the inspector. Mark partial or stale observations and show
  “Block —” when no current height is available; a valid height of zero is zero.
- Give every tile a clear name, implementation, readiness indicator, and a
  compact measurement area.
- Show the relevant balance: Lightning local/channel balance or wallet
  spendable balance, always with explicit units. Put on-chain, remote,
  reserved, and pending balances in the inspector.
- Use bounded passive readers for supported implementations. Reuse pinned
  wallet observation contracts and never trigger SDK recovery, managed
  wallet actions, or payments merely to display a balance.
- Refresh measurements through SSE independently of topology structure.
  Update values in place without rebuilding the canvas.
- Use one shared server sampler and cached HTTP snapshots. Reconnects fetch
  current data; slow clients coalesce refreshes.
- Mark unsupported, unavailable, and stale balances clearly. Do not imply a
  missing value is a zero balance or a healthy observation.

## 4. Human-readable tool and action names

- Introduce a shared explicit display-name mapping instead of converting
  underscores into spaces.
- Examples: `proofstorm_lab_exec` → “Run command”,
  `proofstorm_wallet_balance` → “Check wallet balance”, and
  `proofstorm_channel_rebalance` → “Rebalance channel”.
- Use these names consistently in Activity, Sessions, operation details, and
  any tool display. Keep exact tool identifiers available in details/copy.
- Add MCP tool title metadata where supported, preserving callable tool names,
  arguments, and compatibility.
- Keep action labels neutral; show Running, Completed, Failed, and Cancelled
  as separate outcome labels.

## 5. Theme and copy

- Create shared color tokens for surfaces, borders, text, selection, health,
  focus states, and the graph. Develop the dark palette first.
- Add System / Dark / Light preferences. Default to System, follow OS changes
  while selected, and persist explicit overrides without a light-theme flash.
- Remove slogans and repeated instructions, including “Agents build. You
  observe.”, “Your lab, in view”, and “Explore your lab”.
- Remove filler such as “activity tracking”, “declared topology”, and
  “Informational”. Replace paragraph-length empty states with “No labs”,
  “No activity”, or a useful next action.
- Keep concise explanations that prevent a real misunderstanding, such as
  measurement age, resource requests versus usage, or a failed connection.
- Ensure readable contrast, visible keyboard focus, and status labels that
  do not depend on color alone.

## Delivery order

1. Application shell, canvas layout, theme tokens, and panel behavior.
2. System summary, resource hierarchy, and measured data integration.
3. Lab block height, tile balances, live updates, and viewport-state preservation.
4. Friendly action/tool names and a complete copy pass.
5. Browser verification and focused backend/presentation tests.

## Verification

- Inspect a real populated lab at desktop and small-screen sizes in both
  themes; confirm the canvas occupies the main working area.
- Check fullscreen enter/exit, keyboard access, zoom, pan, fit, and selection
  through multiple SSE updates and a reconnect.
- Reconcile totals with cluster container metrics and expanded rows; exercise
  missing metrics, stopped containers, multiple labs, and shared workloads.
- Verify balance units and passive readers against known results and ensure
  permission checks still apply to cached data after access is revoked.
- Verify that the displayed lab height is the maximum current observed node
  height, stays scoped to that lab, updates after mining, and handles missing
  nodes, reconnects, height zero, and a decrease after reset or reorganization.
- Check automatic OS theme changes and persistence of manual overrides.
- Check tool display names while keeping MCP identifiers unchanged.
- Run the relevant native tests, Wasm compilation, formatting, and lint checks.

## Current implementation status

Initial edits from the original request are in progress: shared measurement
types, a background sampler, cached System snapshots, SSE refreshes, resource
tables, balance display hooks, fullscreen state, and some copy cleanup.
These are a draft foundation, not a completed or browser-verified redesign.
The revised canvas layout, expandable component hierarchy, system-aware theme,
and shared tool display names remain to be implemented under this plan.
