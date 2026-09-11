import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';

/** Applies the same completeness requirement to rules-only, fallback and TSR-only acceptance. */
export function tableCoverage(total, recovered, allowUnresolved = false) {
  if (!allowUnresolved) assert.equal(recovered, total, `Only ${recovered}/${total} tables recovered`);
  return recovered === total ? 'passed' : 'partial';
}

// Run the gate independently without requiring a browser or downloaded models.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  assert.throws(() => tableCoverage(20, 19));
  assert.equal(tableCoverage(20, 19, true), 'partial');
  assert.equal(tableCoverage(20, 20), 'passed');
  console.log('Table completeness checks passed');
}
