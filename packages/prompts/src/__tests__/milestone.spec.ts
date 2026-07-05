import { afterEach, describe, expect, it } from 'vitest';

import { milestone, promptMilestone } from '../milestone.js';

const originalEnv = process.env.VP_EMIT_MILESTONES;

afterEach(() => {
  if (originalEnv === undefined) {
    delete process.env.VP_EMIT_MILESTONES;
  } else {
    process.env.VP_EMIT_MILESTONES = originalEnv;
  }
});

describe('milestone', () => {
  it('emits nothing unless VP_EMIT_MILESTONES=1', () => {
    delete process.env.VP_EMIT_MILESTONES;
    expect(milestone('vp')).toBe('');
    process.env.VP_EMIT_MILESTONES = '0';
    expect(milestone('vp')).toBe('');
  });

  it('encodes the name as a hex OSC 8 hyperlink with a zero-width anchor', () => {
    process.env.VP_EMIT_MILESTONES = '1';
    // "vp" is 0x76 0x70; the sequence must match vite-task's
    // pty_terminal_test_client protocol byte for byte.
    expect(milestone('vp')).toBe('\x1b]8;;https://milestone.invalid/7670\x1b\\​\x1b]8;;\x1b\\');
  });

  it('formats prompt milestones as <kind>:<id>:<state>', () => {
    process.env.VP_EMIT_MILESTONES = '1';
    expect(promptMilestone('select', 'template', '1')).toBe(milestone('select:template:1'));
    expect(promptMilestone('confirm', undefined, 'yes')).toBe(milestone('confirm:confirm:yes'));
  });
});
