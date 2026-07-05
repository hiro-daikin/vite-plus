/**
 * Render-milestone markers for the PTY snapshot harness
 * (crates/vite_cli_snapshots): invisible OSC 8 hyperlinks the harness
 * synchronizes on via `expect-milestone` interactions. Emission is gated on
 * `VP_EMIT_MILESTONES=1`, which only the harness sets; real terminals and
 * piped output never see these bytes.
 *
 * Protocol (shared with vite-task's `pty_terminal_test_client` crate):
 * `OSC 8 ; ; https://milestone.invalid/<hex(name)> ST <ZWSP> OSC 8 ; ; ST`.
 * The zero-width anchor keeps the hyperlink observable through Windows
 * ConPTY, which can drop zero-length hyperlinks.
 */

const MILESTONE_URI_PREFIX = 'https://milestone.invalid/';
const OSC8_OPEN = '\x1b]8;;';
const ST = '\x1b\\';
const ZERO_WIDTH_ANCHOR = '​';

export function milestonesEnabled(): boolean {
  return process.env.VP_EMIT_MILESTONES === '1';
}

/**
 * Returns the encoded milestone byte sequence for `name`, or an empty string
 * when emission is disabled. Append the result to a rendered prompt frame so
 * the marker arrives in the output stream together with the render it marks.
 */
export function milestone(name: string): string {
  if (!milestonesEnabled()) {
    return '';
  }
  let hex = '';
  for (const byte of Buffer.from(name, 'utf8')) {
    hex += byte.toString(16).padStart(2, '0');
  }
  return `${OSC8_OPEN}${MILESTONE_URI_PREFIX}${hex}${ST}${ZERO_WIDTH_ANCHOR}${OSC8_OPEN}${ST}`;
}

/**
 * Milestone for a prompt render, following the harness naming convention
 * `<kind>:<id>:<state>` (e.g. `select:template:1`, `confirm:approve:yes`,
 * `text:project-name:my-app`). `id` defaults to the prompt kind; pass
 * `testId` at ambiguous call sites (multiple prompts of one kind in a flow).
 */
export function promptMilestone(kind: string, id: string | undefined, state: string): string {
  return milestone(`${kind}:${id ?? kind}:${state}`);
}
