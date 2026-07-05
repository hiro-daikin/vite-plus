/**
 * One-click migration of old snap-test cases (steps.json + fixture files) to
 * the new PTY snapshot harness (crates/vite_cli_snapshots fixtures with
 * snapshots.toml), following the mapping in rfcs/interactive-snapshot-tests.md.
 *
 * Usage:
 *   tool migrate-snap-tests packages/cli/snap-tests --vp local [name-filter] [--keep-old]
 *   tool migrate-snap-tests packages/cli/snap-tests-global --vp global [name-filter] [--keep-old]
 *
 * Successfully converted case directories are removed from the old tree, so
 * every case lives in exactly one tree (git has the history; --keep-old
 * defers the removal). Old snap.txt files are not converted; record new
 * baselines afterwards with `UPDATE_SNAPSHOTS=1 just snapshot-test <filter>`
 * and review them against the deleted snap.txt in `git diff`.
 */
import fs from 'node:fs';
import path from 'node:path';

// The legacy steps.json schema comes straight from the old runner, so the
// migrator can never drift from what actually ran.
import type { Steps as OldSteps } from './snap-test.ts';

interface NewStep {
  argv: string[];
  comment?: string;
  cwd?: string;
  envs?: [string, string][];
}

/** New-harness fixture/case names allow only `[A-Za-z0-9_]`. */
function fixtureName(caseName: string): string {
  return caseName.replaceAll(/[^A-Za-z0-9_]/g, '_');
}

interface CaseReport {
  name: string;
  notes: string[];
  todos: string[];
  /** True when nothing was written (e.g. target fixture already exists). */
  skipped?: boolean;
}

const OS_MAP: Record<string, string> = {
  win32: 'windows',
  darwin: 'macos',
  linux: 'linux',
};

/** Splits a shell line on a top-level `&&`, respecting quotes. */
function splitOnAndAnd(line: string): string[] {
  const parts: string[] = [];
  let current = '';
  let quote: string | null = null;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (quote) {
      current += ch;
      if (ch === quote) {
        quote = null;
      }
      continue;
    }
    if (ch === "'" || ch === '"') {
      quote = ch;
      current += ch;
      continue;
    }
    if (ch === '&' && line[i + 1] === '&') {
      parts.push(current);
      current = '';
      i++;
      continue;
    }
    current += ch;
  }
  parts.push(current);
  return parts.map((p) => p.trim()).filter((p) => p.length > 0);
}

/** Tokenizes a simple shell command (no operators), handling quotes. */
function tokenize(command: string): string[] | null {
  const tokens: string[] = [];
  let current = '';
  let hasCurrent = false;
  let i = 0;
  while (i < command.length) {
    const ch = command[i];
    if (ch === ' ' || ch === '\t') {
      if (hasCurrent) {
        tokens.push(current);
        current = '';
        hasCurrent = false;
      }
      i++;
      continue;
    }
    if (ch === "'") {
      const end = command.indexOf("'", i + 1);
      if (end === -1) {
        return null;
      }
      current += command.slice(i + 1, end);
      hasCurrent = true;
      i = end + 1;
      continue;
    }
    if (ch === '"') {
      const end = command.indexOf('"', i + 1);
      if (end === -1) {
        return null;
      }
      current += command.slice(i + 1, end);
      hasCurrent = true;
      i = end + 1;
      continue;
    }
    current += ch;
    hasCurrent = true;
    i++;
  }
  if (hasCurrent) {
    tokens.push(current);
  }
  return tokens;
}

/** Extracts a trailing ` # comment` (outside quotes) from a command line. */
function extractComment(line: string): { command: string; comment?: string } {
  const trimmed = line.trimStart();
  if (trimmed.startsWith('#')) {
    // Comment-only entries are documentation, not commands.
    return { command: '', comment: trimmed.slice(1).trim() };
  }
  let quote: string | null = null;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (quote) {
      if (ch === quote) {
        quote = null;
      }
      continue;
    }
    if (ch === "'" || ch === '"') {
      quote = ch;
      continue;
    }
    if (ch === '#' && i > 0 && (line[i - 1] === ' ' || line[i - 1] === '\t')) {
      return { command: line.slice(0, i).trim(), comment: line.slice(i + 1).trim() };
    }
  }
  return { command: line.trim() };
}

const PASSTHROUGH_PROGRAMS = new Set([
  'vp',
  'vpr',
  'vpx',
  'vpt',
  'oxfmt',
  'oxlint',
  'node',
  'git',
  'npm',
  'pnpm',
  'yarn',
  'bun',
]);

const COREUTILS_MAP: Record<string, string> = {
  cat: 'print-file',
  ls: 'list-dir',
  touch: 'touch-file',
};

/** Programs whose name and args map to the identically-named vpt subcommand. */
const VPT_VERBATIM = new Set(['mkdir', 'rm', 'cp']);

/** New steps run without a shell, so glob patterns would be passed literally. */
function hasGlob(args: string[]): boolean {
  return args.some((a) => /[*?[\]]/.test(a));
}

interface TranslationContext {
  todos: string[];
  notes: string[];
  localRegistry: boolean;
}

/** Records a hand-conversion TODO and returns the placeholder step for it. */
function makeTodo(ctx: TranslationContext, reason: string, command: string): NewStep {
  ctx.todos.push(`${reason}: \`${command}\``);
  return {
    argv: ['vpt', 'print', 'TODO(migrate)'],
    comment: `TODO(migrate) ${reason}: ${command}`,
  };
}

/**
 * Translates one simple (operator-free) shell command into a step, or returns
 * a TODO step preserving the raw text for hand conversion.
 */
function translateSimple(command: string, ctx: TranslationContext): NewStep | null {
  const todo = (reason: string): NewStep => makeTodo(ctx, reason, command);

  // `echo/printf ... > file` (single `>`, not append) becomes an explicit
  // write-file; every other operator or redirect form needs hand conversion.
  // `echo` appends the newline it would have written; `printf` is exact but
  // only when the content carries no escape/format sequences.
  const redirect = command.includes('>>')
    ? null
    : command.match(/^(echo|printf)\s+(.+?)\s*>\s*(\S+)$/);
  if (redirect) {
    const contentTokens = tokenize(redirect[2]);
    if (contentTokens) {
      const content = contentTokens.join(' ');
      if (redirect[1] === 'echo') {
        return { argv: ['vpt', 'write-file', redirect[3], `${content}\n`] };
      }
      if (!/[\\%]/.test(content)) {
        return { argv: ['vpt', 'write-file', redirect[3], content] };
      }
      return todo('printf escape sequences need hand conversion');
    }
  }
  if (/[|;`]|\$\(|<|>>/.test(command)) {
    return todo('shell operators need hand conversion');
  }
  if (command.includes('>')) {
    return todo('redirect needs hand conversion');
  }

  const tokens = tokenize(command);
  if (!tokens || tokens.length === 0) {
    return todo('unparsable command');
  }

  // Leading VAR=value assignments become step envs. Values needing shell
  // expansion ($(pwd), $PATH, backticks) cannot be represented statically.
  const envs: [string, string][] = [];
  while (tokens.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[0])) {
    const [key, ...rest] = tokens.shift()!.split('=');
    const value = rest.join('=');
    if (/[$`]/.test(value)) {
      return todo(`env value for ${key} needs shell expansion`);
    }
    envs.push([key, value]);
  }
  if (tokens.length === 0) {
    return todo('env-only command');
  }

  // `node $SNAP_LOCAL_REGISTRY -- <cmd>` wrapper: unwrap and flag the case.
  if (tokens[0] === 'node' && tokens[1] === '$SNAP_LOCAL_REGISTRY') {
    ctx.localRegistry = true;
    const sep = tokens.indexOf('--');
    const inner = tokens.slice(sep === -1 ? 2 : sep + 1).join(' ');
    const innerStep = translateSimple(inner, ctx);
    if (innerStep && envs.length > 0) {
      innerStep.envs = [...envs, ...(innerStep.envs ?? [])];
    }
    return innerStep;
  }

  if (tokens.some((t) => t.includes('$'))) {
    return todo('shell variable expansion needs hand conversion');
  }

  const program = tokens[0];
  const args = tokens.slice(1);

  let step: NewStep | null = null;
  if (PASSTHROUGH_PROGRAMS.has(program)) {
    step = { argv: tokens };
  } else if (program in COREUTILS_MAP || VPT_VERBATIM.has(program) || program === 'chmod') {
    if (hasGlob(args)) {
      return todo('glob expansion needs hand conversion');
    }
    if (program === 'chmod') {
      // vpt chmod accepts an octal mode or the common `+x` form only.
      if (args.length === 2 && /^([0-7]{3,4}|\+x)$/.test(args[0])) {
        step = { argv: ['vpt', 'chmod', ...args] };
      } else {
        return todo('unsupported chmod invocation');
      }
    } else if (program in COREUTILS_MAP) {
      step = { argv: ['vpt', COREUTILS_MAP[program], ...args.filter((a) => !a.startsWith('-'))] };
    } else {
      step = { argv: ['vpt', program, ...args] };
    }
  } else if (program === 'json-edit') {
    // The legacy repo helper also accepted assignment expressions
    // (`json-edit pkg.json '_.dependencies = {}'`); vpt json-edit is
    // strictly `<file> <dot-path> <value>`.
    if (args.length === 3 && !args[1].includes('=') && !args[1].includes(' ')) {
      step = { argv: ['vpt', 'json-edit', ...args] };
    } else {
      return todo('legacy json-edit expression needs hand conversion');
    }
  } else if (program === 'echo') {
    step = { argv: ['vpt', 'print', args.join(' ')] };
  } else if (program === 'test') {
    // `test -f x` style existence checks map to stat-file, which prints an
    // explicit exists/missing line (a stronger assertion than exit codes).
    // `!` only flips the exit code; stat-file records the actual
    // exists/missing state either way, so it is dropped from the paths.
    const paths = args.filter((a) => a !== '!' && !a.startsWith('-'));
    if (
      paths.length > 0 &&
      args.every((a) => a === '!' || /^-[fde]$/.test(a) || !a.startsWith('-'))
    ) {
      step = { argv: ['vpt', 'stat-file', ...paths] };
    } else {
      return todo('unsupported test expression');
    }
  } else if (program === 'true') {
    ctx.notes.push(`dropped no-op step: \`${command}\``);
    return null;
  } else {
    return todo(`program \`${program}\` is not allowed as a step`);
  }

  if (step && envs.length > 0) {
    step.envs = envs;
  }
  return step;
}

/** Translates one old command line into zero or more new steps. */
function translateCommand(raw: string, ctx: TranslationContext): NewStep[] {
  const { command, comment } = extractComment(raw);
  if (command.length === 0) {
    if (comment) {
      ctx.notes.push(`dropped comment-only command: \`# ${comment}\``);
    }
    return [];
  }
  if (command.includes('||')) {
    return [makeTodo(ctx, '`||` chain needs hand conversion', command)];
  }
  const steps: NewStep[] = [];
  const parts = splitOnAndAnd(command);

  // Special-case `test -f x && echo ...`: the stat-file line already asserts
  // existence, the echo added no information.
  if (parts.length === 2 && /^test\s/.test(parts[0]) && /^echo\s/.test(parts[1])) {
    const step = translateSimple(parts[0], ctx);
    if (step) {
      if (comment) {
        step.comment = comment;
      }
      ctx.notes.push(`folded \`&& echo\` into stat-file assertion: \`${command}\``);
      return [step];
    }
  }

  // `cd <dir> && ...` scopes the rest of the chain to that directory (each
  // legacy command line started fresh at the fixture root, so the cwd never
  // leaks across lines).
  let cwd: string | undefined;
  for (const part of parts) {
    if (/^cd(\s|$)/.test(part)) {
      const cdTokens = tokenize(part);
      const dir = cdTokens?.length === 2 ? cdTokens[1] : null;
      if (!dir || dir.startsWith('/') || /[$`]/.test(dir)) {
        return [makeTodo(ctx, '`cd` form needs hand conversion', command)];
      }
      cwd = cwd === undefined ? dir : `${cwd}/${dir}`;
      continue;
    }
    const step = translateSimple(part, ctx);
    if (step) {
      if (cwd !== undefined) {
        step.cwd = cwd;
      }
      steps.push(step);
    }
  }
  if (comment && steps.length > 0) {
    steps[0].comment = steps[0].comment ? `${comment}; ${steps[0].comment}` : comment;
  }
  return steps;
}

function tomlString(value: string): string {
  return JSON.stringify(value);
}

function tomlKey(key: string): string {
  return /^[A-Za-z0-9_-]+$/.test(key) ? key : tomlString(key);
}

function emitStep(step: NewStep, extra: { timeout?: number; snapshot?: boolean }): string {
  const isSimple =
    step.comment === undefined &&
    step.cwd === undefined &&
    step.envs === undefined &&
    extra.timeout === undefined &&
    extra.snapshot === undefined;
  const argv = `[${step.argv.map(tomlString).join(', ')}]`;
  if (isSimple) {
    return `  ${argv},`;
  }
  const fields = [`argv = ${argv}`];
  if (step.cwd !== undefined) {
    fields.push(`cwd = ${tomlString(step.cwd)}`);
  }
  if (step.comment !== undefined) {
    fields.push(`comment = ${tomlString(step.comment)}`);
  }
  if (step.envs !== undefined) {
    const envs = step.envs.map(([k, v]) => `[${tomlString(k)}, ${tomlString(v)}]`).join(', ');
    fields.push(`envs = [${envs}]`);
  }
  if (extra.timeout !== undefined) {
    fields.push(`timeout = ${extra.timeout}`);
  }
  if (extra.snapshot !== undefined) {
    fields.push(`snapshot = ${String(extra.snapshot)}`);
  }
  return `  { ${fields.join(', ')} },`;
}

function migrateCase(
  caseDir: string,
  caseName: string,
  flavor: string,
  outDir: string,
): CaseReport {
  const report: CaseReport = { name: caseName, notes: [], todos: [] };

  // Never clobber an existing fixture: the same case name can exist in both
  // legacy trees (local and global), and merging those is a hand decision
  // (usually a second [[case]] or a vp = ["local", "global"] matrix).
  const targetDir = path.join(outDir, fixtureName(caseName));
  if (fs.existsSync(targetDir)) {
    report.todos.push(
      `target fixture \`${path.basename(targetDir)}\` already exists; case skipped, merge it by hand`,
    );
    report.skipped = true;
    return report;
  }

  const old: OldSteps = JSON.parse(fs.readFileSync(path.join(caseDir, 'steps.json'), 'utf8'));
  const ctx: TranslationContext = {
    todos: report.todos,
    notes: report.notes,
    localRegistry: false,
  };

  const newName = fixtureName(caseName);
  if (newName !== caseName) {
    report.notes.push(`renamed to \`${newName}\` (identifier rule)`);
  }

  const lines: string[] = [
    '[[case]]',
    `name = ${tomlString(newName)}`,
    `vp = ${tomlString(flavor)}`,
  ];

  if (old.ignoredPlatforms && old.ignoredPlatforms.length > 0) {
    const filters = old.ignoredPlatforms.map((filter) => {
      if (typeof filter === 'string') {
        const os = OS_MAP[filter];
        if (!os) {
          report.todos.push(`unknown ignoredPlatforms value: ${filter}`);
        }
        return tomlString(os ?? filter);
      }
      const os = OS_MAP[filter.os] ?? filter.os;
      const libc = filter.libc ? `, libc = ${tomlString(filter.libc)}` : '';
      return `{ os = ${tomlString(os)}${libc} }`;
    });
    lines.push(`skip-platforms = [${filters.join(', ')}]`);
  }

  if (old.env && Object.keys(old.env).length > 0) {
    const sets = Object.entries(old.env).filter(([, v]) => v !== '');
    const unsets = Object.entries(old.env)
      .filter(([, v]) => v === '')
      .map(([k]) => k);
    if (sets.length > 0) {
      const table = sets.map(([k, v]) => `${tomlKey(k)} = ${tomlString(v)}`).join(', ');
      lines.push(`env = { ${table} }`);
    }
    if (unsets.length > 0) {
      lines.push(`unset-env = [${unsets.map(tomlString).join(', ')}]`);
      report.notes.push(`empty-string env entries became unset-env: ${unsets.join(', ')}`);
    }
  }

  if (old.serial) {
    report.notes.push('dropped `serial: true` (per-case VP_HOME isolation replaces it)');
  }
  if (old.linkCheckoutPackages) {
    report.todos.push('`linkCheckoutPackages` is not supported by the new harness yet');
  }

  const stepLines: string[] = [];
  for (const entry of old.commands) {
    const raw = typeof entry === 'string' ? entry : entry.command;
    const extra =
      typeof entry === 'string'
        ? {}
        : {
            timeout: entry.timeout,
            snapshot: entry.ignoreOutput === true ? false : undefined,
          };
    for (const step of translateCommand(raw, ctx)) {
      stepLines.push(emitStep(step, extra));
    }
  }
  if (old.localVitePlusPackages || ctx.localRegistry) {
    lines.push('local-registry = true');
    report.todos.push('`local-registry` cases are not supported by the new harness yet');
  }
  lines.push('steps = [', ...stepLines, ']');

  if (old.after && old.after.length > 0) {
    const afterLines: string[] = [];
    for (const raw of old.after) {
      for (const step of translateCommand(raw, ctx)) {
        afterLines.push(emitStep(step, {}));
      }
    }
    lines.push('after = [', ...afterLines, ']');
  }

  // Write the fixture: everything except steps.json and snap.txt carries over.
  const fixtureDir = path.join(outDir, newName);
  fs.mkdirSync(fixtureDir, { recursive: true });
  fs.cpSync(caseDir, fixtureDir, {
    recursive: true,
    filter: (src) => {
      const base = path.basename(src);
      return base !== 'steps.json' && base !== 'snap.txt';
    },
  });
  fs.writeFileSync(path.join(fixtureDir, 'snapshots.toml'), `${lines.join('\n')}\n`);
  return report;
}

export function migrateSnapTests(): void {
  const args = process.argv.slice(3);
  const positional: string[] = [];
  let flavor: string | undefined;
  let keepOld = false;
  let outDir = 'crates/vite_cli_snapshots/tests/cli_snapshots/fixtures';
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--vp') {
      flavor = args[++i];
    } else if (args[i] === '--out') {
      outDir = args[++i];
    } else if (args[i] === '--keep-old') {
      keepOld = true;
    } else {
      positional.push(args[i]);
    }
  }
  const [oldDir, nameFilter] = positional;
  if (!oldDir || (flavor !== 'local' && flavor !== 'global')) {
    console.error(
      'Usage: tool migrate-snap-tests <old-snap-tests-dir> --vp <local|global> [name-filter] [--out <fixtures-dir>] [--keep-old]',
    );
    process.exit(1);
  }

  const caseDirs = fs
    .readdirSync(oldDir, { withFileTypes: true })
    .filter((e) => e.isDirectory() && !e.name.startsWith('.'))
    .map((e) => e.name)
    .filter((name) => (nameFilter ? name.includes(nameFilter) : true))
    .toSorted();

  const reports: CaseReport[] = [];
  for (const name of caseDirs) {
    const caseDir = path.join(oldDir, name);
    const report = migrateCase(caseDir, name, flavor, outDir);
    reports.push(report);
    // Only cleanly converted cases leave the legacy tree: TODO placeholders
    // are not coverage, so those cases keep their old dir until the hand
    // conversion lands.
    if (!keepOld && !report.skipped && report.todos.length === 0) {
      fs.rmSync(caseDir, { recursive: true, force: true });
    } else if (!keepOld && !report.skipped && report.todos.length > 0) {
      report.notes.push('old case dir kept until the TODOs are hand-converted');
    }
  }

  const reportLines: string[] = [
    '# Snap-test migration report',
    '',
    `Source: \`${oldDir}\` (flavor: ${flavor}), ${reports.length} case(s).`,
    '',
    'Record baselines with `UPDATE_SNAPSHOTS=1 just snapshot-test <filter>`',
    'and review each new snapshot against the deleted snap.txt in `git diff`.',
    ...(keepOld
      ? ['The old case directories were kept (--keep-old); delete them in the same PR.']
      : ['The old case directories were removed (recover with `git checkout -- <dir>`).']),
    '',
  ];
  let todoCount = 0;
  for (const report of reports) {
    reportLines.push(`## ${report.name}`);
    if (report.todos.length === 0 && report.notes.length === 0) {
      reportLines.push('', 'auto-migrated cleanly', '');
      continue;
    }
    reportLines.push('');
    for (const todo of report.todos) {
      reportLines.push(`- TODO: ${todo}`);
      todoCount++;
    }
    for (const note of report.notes) {
      reportLines.push(`- note: ${note}`);
    }
    reportLines.push('');
  }
  // The report lives next to the fixtures dir, not inside it: everything
  // inside `fixtures/` is treated as a fixture by the harness.
  const reportPath = path.join(outDir, '..', 'MIGRATION-REPORT.md');
  fs.writeFileSync(reportPath, reportLines.join('\n'));
  const migrated = reports.filter((r) => !r.skipped).length;
  const skipped = reports.length - migrated;
  const removed = keepOld ? 0 : reports.filter((r) => !r.skipped && r.todos.length === 0).length;
  console.log(
    `Migrated ${migrated} case(s) to ${outDir}${
      removed > 0 ? ` and removed ${removed} cleanly converted old case dir(s)` : ''
    }${skipped > 0 ? `, skipped ${skipped}` : ''}; ${todoCount} TODO(s) need hand conversion.`,
  );
  console.log(`Report: ${reportPath}`);
}

// Exported for unit tests only.
export { fixtureName, translateCommand };
export type { NewStep, TranslationContext };
