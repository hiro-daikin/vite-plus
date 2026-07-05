import { describe, expect, it } from 'vitest';

import {
  fixtureName,
  translateCommand,
  type NewStep,
  type TranslationContext,
} from '../migrate-snap-tests.ts';

function ctx(): TranslationContext {
  return { todos: [], notes: [], localRegistry: false };
}

function argvs(steps: NewStep[]): string[][] {
  return steps.map((s) => s.argv);
}

function isTodo(steps: NewStep[]): boolean {
  return steps.length === 1 && steps[0].comment?.startsWith('TODO(migrate)') === true;
}

describe('fixtureName', () => {
  it('normalizes every invalid identifier character', () => {
    expect(fixtureName('migration-not-supported-npm8.2')).toBe('migration_not_supported_npm8_2');
    expect(fixtureName('check-pass')).toBe('check_pass');
  });
});

describe('translateCommand', () => {
  it('drops `!` from test expressions (stat-file records actual state)', () => {
    const steps = translateCommand('test ! -f .nvmrc', ctx());
    expect(argvs(steps)).toEqual([['vpt', 'stat-file', '.nvmrc']]);
  });

  it('passes octal and +x chmod through, TODOs other symbolic modes', () => {
    expect(argvs(translateCommand('chmod 755 hook.mjs', ctx()))).toEqual([
      ['vpt', 'chmod', '755', 'hook.mjs'],
    ]);
    expect(argvs(translateCommand('chmod +x hook.mjs', ctx()))).toEqual([
      ['vpt', 'chmod', '+x', 'hook.mjs'],
    ]);
    expect(isTodo(translateCommand('chmod u+rw hook.mjs', ctx()))).toBe(true);
  });

  it('TODOs glob arguments for shell-less vpt file verbs', () => {
    expect(isTodo(translateCommand('rm -rf *.tgz', ctx()))).toBe(true);
    expect(isTodo(translateCommand('cat dist/*.js', ctx()))).toBe(true);
  });

  it('appends the newline echo would have written to redirected files', () => {
    const steps = translateCommand('echo hello > out.txt', ctx());
    expect(argvs(steps)).toEqual([['vpt', 'write-file', 'out.txt', 'hello\n']]);
  });

  it('keeps printf redirects exact, TODOs escape sequences', () => {
    expect(argvs(translateCommand("printf 'plain text' > out.txt", ctx()))).toEqual([
      ['vpt', 'write-file', 'out.txt', 'plain text'],
    ]);
    expect(isTodo(translateCommand("printf 'a\\nb' > out.txt", ctx()))).toBe(true);
  });

  it('accepts dot-path json-edit and TODOs legacy expression syntax', () => {
    expect(
      argvs(translateCommand("json-edit package.json scripts.build 'vp build'", ctx())),
    ).toEqual([['vpt', 'json-edit', 'package.json', 'scripts.build', 'vp build']]);
    expect(isTodo(translateCommand("json-edit package.json '_.dependencies = {}'", ctx()))).toBe(
      true,
    );
  });

  it('TODOs env assignments that need shell expansion', () => {
    expect(
      isTodo(translateCommand('NPM_CONFIG_PREFIX=$(pwd)/prefix npm install -g x', ctx())),
    ).toBe(true);
    expect(isTodo(translateCommand('PATH=$PATH vp check', ctx()))).toBe(true);
  });

  it('turns leading cd chains into step cwd', () => {
    const steps = translateCommand('cd packages/web && vp run build', ctx());
    expect(steps).toHaveLength(1);
    expect(steps[0].argv).toEqual(['vp', 'run', 'build']);
    expect(steps[0].cwd).toBe('packages/web');
  });

  it('TODOs cd forms it cannot represent', () => {
    expect(isTodo(translateCommand('cd /tmp && vp check', ctx()))).toBe(true);
    expect(isTodo(translateCommand('cd $DIR && vp check', ctx()))).toBe(true);
  });
});
