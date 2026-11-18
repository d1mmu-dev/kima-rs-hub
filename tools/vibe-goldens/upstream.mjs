// Reads upstream's TypeScript sources for the golden generator and the benches from IMMUTABLE git objects of the pinned
// commit, never from a working tree: a freshly pulled reference checkout (or one with local edits) cannot silently
// change what the goldens are computed from. The checkout only has to contain the pin commit.
//   UPSTREAM            path of a kima-hub git checkout containing the pin (default: KIMA_HUB_REFERENCE from the
//                       environment or the private tools config, tools/tools-env.mjs)
// Inherited repository variables (GIT_DIR, GIT_WORK_TREE, ...) are removed for these git calls.
import { execFileSync } from 'node:child_process';
import { toolsEnv } from '../tools-env.mjs';

export const PIN = 'c7527577d925b20f5adf410c7fd4e0c8ac250f69';

function gitEnv() {
  const env = { ...process.env };
  const local = execFileSync('git', ['rev-parse', '--local-env-vars'], { encoding: 'utf8', env: { PATH: process.env.PATH } });
  for (const name of local.split('\n').filter(Boolean)) delete env[name];
  return env;
}

export function upstreamRoot() {
  const root = process.env.UPSTREAM || toolsEnv('KIMA_HUB_REFERENCE');
  if (!root) throw new Error('set UPSTREAM (or KIMA_HUB_REFERENCE) to a kima-hub git checkout containing ' + PIN);
  return root;
}

function git(args) {
  return execFileSync('git', ['-C', upstreamRoot(), ...args], { encoding: 'utf8', env: gitEnv(), stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: 256 * 1024 * 1024 });
}

/** The bytes of `relPath` at the pinned commit, as UTF-8 text; throws when the checkout lacks the pin or the path. */
export function readPinned(relPath) {
  try {
    git(['cat-file', '-e', `${PIN}^{commit}`]);
  } catch {
    throw new Error(`${upstreamRoot()} does not contain the pinned commit ${PIN}`);
  }
  try {
    return git(['cat-file', 'blob', `${PIN}:${relPath}`]);
  } catch {
    throw new Error(`${relPath} does not exist at ${PIN}`);
  }
}
