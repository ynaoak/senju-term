/* Self-test for the terminal's filesystem-path matcher (`findPathsInLine` in
 * ui/app.js).
 *
 * The matcher decides which characters of every output line become a clickable
 * link, so a regex that is slightly too greedy underlines punctuation, and one
 * slightly too shy drops the last character of a filename — both invisible in
 * a screenshot and both annoying every single day. The cases below are the
 * shapes real build/test output actually produces.
 *
 * app.js is a plain script, not a module (the LP-and-app rule: no bundler), so
 * the functions are lifted out by the marker comments that bracket them and
 * evaluated here. If those markers move, this fails loudly rather than
 * silently testing nothing.
 *
 *   node scripts/check-path-links.mjs
 */
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const here = path.dirname(fileURLToPath(import.meta.url));
const appJs = fs.readFileSync(path.join(here, '..', 'ui', 'app.js'), 'utf8');

const BEGIN = 'const PATH_PATTERNS';
const END = '/** Menu rows for one path';
const from = appJs.indexOf(BEGIN);
const to = appJs.indexOf(END);
if (from < 0 || to < 0 || to <= from) {
  console.error(
    `check-path-links: could not lift the matcher out of ui/app.js.\n` +
      `  Looked for ${JSON.stringify(BEGIN)} .. ${JSON.stringify(END)}.\n` +
      `  If that code moved or was renamed, update these markers — do not delete the check.`
  );
  process.exit(1);
}
const findPathsInLine = new Function(`${appJs.slice(from, to)}; return findPathsInLine;`)();

// `pathFromSelection` decides what the right-click menu acts on, and it sits
// below the lifted span, so it is pulled out on its own.
const SEL_BEGIN = 'function pathFromSelection(term) {';
const selFrom = appJs.indexOf(SEL_BEGIN);
const selTo = selFrom < 0 ? -1 : appJs.indexOf('\n}\n', selFrom);
if (selFrom < 0 || selTo < 0) {
  console.error(
    `check-path-links: could not lift ${JSON.stringify(SEL_BEGIN)} out of ui/app.js.\n` +
      `  If that function moved or was renamed, update this marker — do not delete the check.`
  );
  process.exit(1);
}
const pathFromSelection = new Function(
  `${appJs.slice(from, to)}\n${appJs.slice(selFrom, selTo + 2)}; return pathFromSelection;`
)();

const CASES = [
  // Plain absolute paths.
  ['/usr/local/bin/senju', ['/usr/local/bin/senju']],
  ['"quoted /var/log/app.log" here', ['/var/log/app.log']],
  ['a=/opt/tool/bin b=/opt/tool/etc', ['/opt/tool/bin', '/opt/tool/etc']],
  // Sentence punctuation is not part of the filename.
  ['error: cannot open /etc/hosts.', ['/etc/hosts']],
  ['wrote (/usr/lib/libfoo.so).', ['/usr/lib/libfoo.so']],
  // rustc/grep locations: the line:col belongs to the message, not the file.
  ['  --> /home/user/senju-term/src/lib.rs:42:13', ['/home/user/senju-term/src/lib.rs']],
  // `~` is expanded by the backend, which is the side that knows the home dir.
  ['see ~/work/notes.md for details', ['~/work/notes.md']],
  // Windows drive-absolute and UNC.
  ['C:\\Users\\naoaki\\Documents\\report.docx', ['C:\\Users\\naoaki\\Documents\\report.docx']],
  ['copied to \\\\nas\\share\\backup', ['\\\\nas\\share\\backup']],
  // A URL's path must stay one link (WebLinksAddon's), never two.
  ['open https://example.com/a/b/c now', []],
  ['git clone https://github.com/o/r.git /tmp/r', ['/tmp/r']],
  // Relative paths are deliberately not linked: without cwd tracking they
  // would resolve against the wrong directory.
  ['relative src/lib.rs is not linked', []],
  ['./configure --prefix=/usr/local', ['/usr/local']],
  // Nothing worth linking.
  ['bare / and ~ are skipped', []],
  ['no paths here at all', []],
];

let failed = 0;
for (const [line, want] of CASES) {
  const got = findPathsInLine(line).map((h) => h.text);
  if (JSON.stringify(got) !== JSON.stringify(want)) {
    failed++;
    console.error(`  FAIL ${JSON.stringify(line)}\n       got  ${JSON.stringify(got)}\n       want ${JSON.stringify(want)}`);
  }
}

// The reported offsets are what xterm underlines and hit-tests, so they have
// to slice back to exactly the matched path — a match with the right text but
// the wrong columns puts the link on the neighbouring words.
for (const [line] of CASES) {
  for (const hit of findPathsInLine(line)) {
    const slice = line.slice(hit.start, hit.end);
    if (slice !== hit.text) {
      failed++;
      console.error(
        `  FAIL offsets in ${JSON.stringify(line)}\n       [${hit.start},${hit.end}) slices to ${JSON.stringify(slice)}, not ${JSON.stringify(hit.text)}`
      );
    }
  }
}

// The right-click menu resolves the path under the pointer by testing the
// clicked column against these same offsets (pathAtPoint in ui/app.js). Getting
// the boundaries wrong there means right-clicking the punctuation *next to* a
// path silently offers to reveal it.
const COLUMN_CASES = [
  // [line, column, expected path]
  ['error in /home/user/app/src/main.rs:42 while building', 9, '/home/user/app/src/main.rs'],
  ['error in /home/user/app/src/main.rs:42 while building', 34, '/home/user/app/src/main.rs'],
  ['error in /home/user/app/src/main.rs:42 while building', 8, ''],   // the space before
  ['error in /home/user/app/src/main.rs:42 while building', 35, ''],  // the ':' after
  ['error in /home/user/app/src/main.rs:42 while building', 0, ''],
  ['no paths on this line at all', 4, ''],
];
for (const [line, col, want] of COLUMN_CASES) {
  const hit = findPathsInLine(line).find((h) => col >= h.start && col < h.end);
  const got = hit ? hit.text : '';
  if (got !== want) {
    failed++;
    console.error(
      `  FAIL column ${col} of ${JSON.stringify(line)}\n       got  ${JSON.stringify(got)}\n       want ${JSON.stringify(want)}`
    );
  }
}

// A selection only counts when it is entirely one path: acting on a path found
// inside a selected sentence would reveal something never pointed at.
const SELECTION_CASES = [
  ['/home/user/x.txt', '/home/user/x.txt'],
  ['  /home/user/x.txt \n', '/home/user/x.txt'],
  ['~/notes.md', '~/notes.md'],
  ['C:\\Users\\me\\a.txt', 'C:\\Users\\me\\a.txt'],
  ['see /home/user/x.txt now', ''],
  ['just words', ''],
  ['', ''],
  // No whitespace, so these reach the "is the selection *exactly* one path"
  // test rather than being rejected before it.
  ['(/tmp/a.txt)', ''],
  ['x/home/user/a.txt', ''],
];
for (const [selection, want] of SELECTION_CASES) {
  const got = pathFromSelection({ getSelection: () => selection });
  if (got !== want) {
    failed++;
    console.error(
      `  FAIL selection ${JSON.stringify(selection)}\n       got  ${JSON.stringify(got)}\n       want ${JSON.stringify(want)}`
    );
  }
}

if (failed) {
  console.error(`path-links self-test: ${failed} FAILED`);
  process.exit(1);
}
console.log(
  `path-links self-test: OK (${CASES.length} lines, ${COLUMN_CASES.length} columns, ${SELECTION_CASES.length} selections)`
);
