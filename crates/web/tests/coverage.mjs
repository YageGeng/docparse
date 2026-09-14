import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';

/** Applies the same completeness requirement to rules-only, fallback and TSR-only acceptance. */
export function tableCoverage(total, recovered, allowUnresolved = false) {
  if (!allowUnresolved) assert.equal(recovered, total, `Only ${recovered}/${total} tables recovered`);
  return recovered === total ? 'passed' : 'partial';
}

/** Uses one accepted mode for both UI selection and all completeness/provenance gates. */
export function validateTableMode(mode) {
  assert(['tsr_only', 'fallback', 'rules_only'].includes(mode), `Unknown table mode: ${mode}`);
  return mode;
}

// Run the gate independently without requiring a browser or downloaded models.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  assert.equal(validateTableMode('tsr_only'), 'tsr_only');
  assert.throws(() => validateTableMode('external_only'));
  assert.equal(validateTableMode('fallback'), 'fallback');
  assert.equal(validateTableMode('rules_only'), 'rules_only');
  assert.throws(() => validateTableMode('unknown'));
  assert.throws(() => tableCoverage(20, 19));
  assert.equal(tableCoverage(20, 19, true), 'partial');
  assert.equal(tableCoverage(20, 20), 'passed');
  console.log('Table completeness checks passed');
}
