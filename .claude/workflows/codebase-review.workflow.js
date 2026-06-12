export const meta = {
  name: 'diffo-codebase-review',
  description: 'Whole-codebase audit of diffo (Zig TUI): build check, fan-out finders, adversarial verify, synthesized report',
  whenToUse: 'Deep, exhaustive review of the diffo Zig codebase as it currently stands (not a diff review).',
  phases: [
    { title: 'Build', detail: 'zig build + zig build test, capture diagnostics' },
    { title: 'Review', detail: 'per-module + cross-cutting finders, all dimensions' },
    { title: 'Verify', detail: 'adversarially confirm/refute each finding (panel for high/critical)' },
    { title: 'Synthesize', detail: 'dedupe, rank, write prioritized report' },
  ],
}

// ---------- schemas ----------
const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        properties: {
          title: { type: 'string' },
          file: { type: 'string' },
          line: { type: 'string', description: 'line number or range, e.g. "120" or "120-135"' },
          dimension: { type: 'string', enum: ['memory', 'correctness', 'robustness', 'performance', 'ffi', 'simplicity', 'build'] },
          severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
          description: { type: 'string', description: 'what is wrong and why it matters' },
          evidence: { type: 'string', description: 'the code snippet / concrete reasoning' },
          suggested_fix: { type: 'string' },
          confidence: { type: 'string', enum: ['high', 'medium', 'low'] },
        },
        required: ['title', 'file', 'line', 'dimension', 'severity', 'description', 'suggested_fix', 'confidence'],
      },
    },
  },
  required: ['findings'],
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    real: { type: 'boolean' },
    severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
    reasoning: { type: 'string' },
    fix_note: { type: 'string' },
  },
  required: ['real', 'severity', 'reasoning'],
}

const BUILD_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    builds: { type: 'boolean' },
    tests_pass: { type: 'boolean' },
    summary: { type: 'string' },
    diagnostics: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        properties: {
          file: { type: 'string' },
          line: { type: 'string' },
          kind: { type: 'string', enum: ['error', 'warning', 'note'] },
          message: { type: 'string' },
        },
        required: ['file', 'kind', 'message'],
      },
    },
  },
  required: ['builds', 'tests_pass', 'summary', 'diagnostics'],
}

// ---------- review units ----------
const UNITS = [
  { label: 'git', files: ['src/git.zig'], focus: 'Git discovery & snapshot loading. Subprocess spawning/argv, reading child stdout/stderr, partial reads, exit-code handling, IO error paths, allocator ownership of snapshot buffers, behavior on non-repo / detached HEAD / empty diff / binary files / huge output.' },
  { label: 'cli', files: ['src/cli.zig'], focus: 'Argument parsing, subcommands, JSON emission for the agent skill, allocator ownership, handling of malformed/missing args and untrusted paths, and whether JSON output is always well-formed and properly escaped.' },
  { label: 'diff', files: ['src/diff.zig'], focus: 'Unified-diff/patch PARSING correctness: hunk headers (@@ -a,b +c,d @@), line-kind classification, counts, off-by-one, integer overflow/underflow on line numbers, slicing bounds on malformed/truncated patches, "\\ No newline at end of file", rename/binary/mode-change headers.' },
  { label: 'inline_diff', files: ['src/inline_diff.zig'], focus: 'Intra-line inline diff algorithm: index/bounds correctness, UTF-8 vs byte handling, allocation lifecycle, behavior on empty/identical/very-long lines, and correctness of emitted highlight ranges.' },
  { label: 'store', files: ['src/store.zig'], focus: 'Review state & comments: serialization/deserialization, the @intCast/@ptrCast sites, ID handling, persistence to disk, ownership/lifetime of comment strings, map/array growth, and consistency under add/remove/update and reload.' },
  { label: 'util', files: ['src/util.zig'], focus: 'Shared helpers and the casts here; correctness & edge cases of each helper, and whether any silently truncates or mishandles boundary values.' },
  { label: 'tui_view', files: ['src/tui_view.zig'], focus: 'Visual rows, fold expand/collapse, change navigation (next/prev change), and scroll/index math. Off-by-one in row<->line mapping, fold boundaries, empty-file and all-context cases.' },
  { label: 'tui_text', files: ['src/tui_text.zig'], focus: 'ANSI-aware cell width & fitting. Unicode width (wide/zero-width/combining/emoji), tab handling, truncation correctness, and that ANSI escape sequences are measured as zero width and never split mid-sequence.' },
  { label: 'theme', files: ['src/theme.zig'], focus: 'Color/theme parsing and the @panic/unreachable sites — can any malformed/untrusted theme or config input reach a panic? Color value bounds and defaults.' },
  { label: 'syntax', files: ['src/syntax.zig', 'src/syntax_cache.zig', 'src/syntax_grammars.zig', 'src/syntax_query.zig'], focus: 'Tree-sitter integration in Zig: the many @ptrCast/@alignCast/@intCast and extern grammar bindings. C-buffer lifetime (does the source slice outlive the tree/cursor that points into it?), null checks on TS API returns, query-capture index bounds, cache key correctness & invalidation, and usize<->C uint32 cast truncation.' },
  { label: 'tree_sitter', files: ['src/tree_sitter.zig'], focus: 'The raw tree-sitter FFI binding layer: extern signature correctness, null handling of parser/tree/node returns, freeing of TS objects (tree/parser/cursor) — leaks or double-free — and pointer/lifetime safety when passing Zig slices to C.' },
  { label: 'tui:render', files: ['src/tui.zig'], focus: 'RENDERING & STYLE only (render, renderDiffLines, renderVisualRowLines, renderStacked/Split code rows, renderCodeText, styleCell, reapplyStyleAfterResets, rowBg/inlineBg/rowFg, applyInlineRanges, wrapAnsiText*, appendRenderedCell). Known pitfalls: syntax highlighting emits ANSI resets and the row background must be reapplied after every reset; status/footer must occupy fixed terminal rows to avoid stale remnants. Also check buffer/index bounds in the wrapping math.' },
  { label: 'tui:input', files: ['src/tui.zig'], focus: 'INPUT, EVENT LOOP & CLIPBOARD only (run, handleEvent, handleMouse, Event/MouseEvent parsing, copySelectionToClipboard, writeToClipboard, trySystemClipboard, tryOsc52, tryPipedCommand, selectedText). Check terminal raw-mode setup/teardown on ALL exit paths (errdefer), child-process spawning for clipboard, escape-sequence/mouse parsing bounds, and that selection ranges are validated before slicing.' },
  { label: 'tui:nav', files: ['src/tui.zig'], focus: 'NAVIGATION, SCROLL/LAYOUT MATH & STATE LIFECYCLE only (State.init/deinit, Layout, Size, moveLine, scrollLines, gotoAction/goto edges, cancelSelectionMode, isSelected, selection state). Check scroll clamping (top/bottom bounds), isize<->usize conversions in delta math, that State is freed exactly once, and cursor/viewport consistency after resize and jumps.' },
  { label: 'x:memory', files: ['ALL'], focus: 'CROSS-MODULE MEMORY OWNERSHIP & LEAKS. Trace who allocates and who frees across module boundaries (main -> tui -> store/diff/git/syntax). Arena vs gpa usage; defer/errdefer correctness on error and early-return paths; allocations leaked when a later step fails; returned slices whose backing memory is freed; ArrayList/HashMap growth without dealloc. Read main.zig and root.zig and the allocation-heavy call sites.' },
  { label: 'x:perf', files: ['ALL'], focus: 'CROSS-MODULE PERFORMANCE of the render/scroll hot path (recent work targeted zero-alloc line counting & O(n) scroll). Look for per-frame heap allocations, O(n^2) loops over lines/rows, repeated re-parsing/re-highlighting, redundant work on every keypress, and anything that scales badly with large diffs/files.' },
  { label: 'x:robust', files: ['ALL'], focus: 'ADVERSARIAL / UNTRUSTED INPUT ROBUSTNESS. What happens with: malformed/truncated git output, enormous files, binary content, invalid UTF-8 in source, extremely long lines, zero-width terminal, terminal resize mid-render, ANSI escapes embedded in source text, and a missing/locked/corrupt store file. Identify crash/panic/UB or corruption paths across diff/git/tui_text/tui.' },
  { label: 'x:design', files: ['ALL'], focus: 'ARCHITECTURE, SIMPLIFICATION, REUSE & DEAD CODE across modules. Duplicated logic that should be shared, overly large functions, leaky abstractions, inconsistent error-handling patterns, unused code/exports, and public API surface that should be tightened. Quality only — not bugs.' },
]

// ---------- prompt builders ----------
function finderPrompt(u) {
  const filesLine = u.files[0] === 'ALL'
    ? 'Scope: the entire src/ tree (all src/*.zig). Read the files relevant to your focus; vendor/ (tree-sitter grammars) is third-party and out of scope.'
    : 'Files to review (Read them fully before judging):\n' + u.files.map((f) => '  - ' + f).join('\n')
  return [
    'You are auditing the codebase of `diffo`, a terminal (TUI) git diff & code-review tool written in Zig 0.16.0.',
    'This is a WHOLE-CODEBASE audit of the code as it is today (NOT a diff/PR review).',
    '',
    'YOUR FOCUS:',
    u.focus,
    '',
    filesLine,
    '',
    'Use Read/Grep/Bash to inspect the actual code; cross-reference callers and callees as needed. This is Zig 0.16 — pay attention to allocator ownership, defer/errdefer on error paths, error-union propagation, integer cast truncation (@intCast/@truncate), slice bounds, and (for FFI) C-pointer lifetimes.',
    '',
    'Report ONLY concrete, evidence-backed issues you can point to in the real code. Favor precision over volume — a few well-grounded findings beat many speculative ones. For each finding give an exact file and line (or range), what is wrong and WHY it matters, the code evidence, a concrete suggested fix, and your confidence.',
    'Severity rubric: critical = memory corruption / UB / crash on normal use, or data loss; high = crash / leak / wrong output on plausible input, or a clear security issue; medium = wrong behavior on edge cases, a notable perf problem, or genuinely fragile code; low = minor correctness/robustness nit or cleanup. Use the `simplicity` dimension (severity low/medium) for pure quality/maintainability items.',
    'If you genuinely find nothing of substance in your scope, return an empty findings array rather than inventing issues.',
  ].join('\n')
}

function verifyPrompt(f, lens) {
  return [
    'You are an adversarial verifier auditing ONE claimed issue in `diffo` (a Zig 0.16 TUI git-diff tool). Confirm or refute it by reading the REAL code — do not trust the claim.',
    '',
    'CLAIMED ISSUE',
    'Title: ' + f.title,
    'Location: ' + f.file + ':' + f.line,
    'Dimension / claimed severity: ' + f.dimension + ' / ' + f.severity,
    'Description: ' + f.description,
    'Evidence given: ' + (f.evidence || '(none)'),
    'Proposed fix: ' + f.suggested_fix,
    '',
    'VERIFICATION LENS: ' + lens,
    '',
    'Open the cited file (plus any callers/callees you need) with Read/Grep. Determine whether the described mechanism is actually true of the code as written. Try to construct a concrete input or program state that triggers it; if you cannot, or the code already guards against it, mark real=false.',
    'Bias toward real=false when you cannot concretely confirm the mechanism in the actual source. If real, give the corrected severity (it may be LOWER than claimed). Be terse and specific in your reasoning.',
  ].join('\n')
}

const PANEL_LENSES = [
  'CORRECTNESS — is the described mechanism actually how this code behaves? Trace the exact control/data flow line by line.',
  'TRIGGERABILITY — find a concrete input, terminal state, or git output that makes it actually happen; if none plausibly exists, refute it.',
  'SEVERITY & IMPACT — assuming it can occur, what is the real-world impact, and is the claimed severity justified or inflated?',
]
const SOLO_LENS = 'Refute if at all possible — verify the mechanism exists in the real code AND can actually be triggered on a plausible input.'

// ---------- helpers ----------
const SEV_RANK = { critical: 0, high: 1, medium: 2, low: 3 }
const CONF_RANK = { high: 0, medium: 1, low: 2 }

function dedupKey(f) {
  return (f.file || '') + '::' + (f.line || '') + '::' + (f.title || '').toLowerCase().replace(/\s+/g, ' ').trim()
}

// ============================================================
phase('Build')
log('Building diffo and running the test suite (Zig 0.16, compiles vendored C — may take a few minutes)...')
const build = await agent(
  [
    'Build and test the `diffo` Zig 0.16.0 project from the repo root. Run `zig build` and then `zig build test`.',
    'The first build compiles vendored tree-sitter C grammars and can take several minutes — allow up to 10 minutes (set a generous Bash timeout).',
    'Capture every compiler error, warning, and relevant note with file:line and message. Report whether it builds and whether all tests pass. Do NOT modify or fix anything.',
  ].join('\n'),
  { label: 'build+test', phase: 'Build', schema: BUILD_SCHEMA },
)
log('Build: ' + (build && build.builds ? 'OK' : 'FAILED') + ' | Tests: ' + (build && build.tests_pass ? 'PASS' : 'FAIL/UNKNOWN') + ' | diagnostics: ' + ((build && build.diagnostics ? build.diagnostics.length : 0)))

// ============================================================
phase('Review')
log('Fanning out ' + UNITS.length + ' reviewers (per-module + cross-cutting) across all dimensions...')
const finderResults = await parallel(
  UNITS.map((u) => () => agent(finderPrompt(u), { label: 'review:' + u.label, phase: 'Review', schema: FINDINGS_SCHEMA })),
)

// attach source unit, flatten, dedupe
let raw = []
finderResults.forEach((r, i) => {
  const fs = r && r.findings ? r.findings : []
  fs.forEach((f) => raw.push({ ...f, unit: UNITS[i].label }))
})
const seen = new Set()
let deduped = []
for (const f of raw) {
  const k = dedupKey(f)
  if (seen.has(k)) continue
  seen.add(k)
  deduped.push(f)
}
deduped.sort((a, b) => (SEV_RANK[a.severity] - SEV_RANK[b.severity]) || (CONF_RANK[a.confidence] - CONF_RANK[b.confidence]))

const CAP = 150
let toVerify = deduped
if (deduped.length > CAP) {
  toVerify = deduped.slice(0, CAP)
  log('NOTE: ' + deduped.length + ' findings exceeded verification cap of ' + CAP + '; verifying the ' + CAP + ' most severe. ' + (deduped.length - CAP) + ' lower-severity findings dropped.')
}
log(raw.length + ' raw findings -> ' + deduped.length + ' after dedupe -> verifying ' + toVerify.length + '.')

// ============================================================
phase('Verify')
const verified = await parallel(
  toVerify.map((f) => () => {
    const isHot = f.severity === 'critical' || f.severity === 'high'
    if (isHot) {
      return parallel(PANEL_LENSES.map((lens) => () => agent(verifyPrompt(f, lens), { label: 'verify:' + f.unit, phase: 'Verify', schema: VERDICT_SCHEMA })))
        .then((votes) => {
          const v = votes.filter(Boolean)
          const realVotes = v.filter((x) => x.real)
          const real = realVotes.length >= 2
          // corrected severity: median rank among real votes, else keep claimed
          let sev = f.severity
          if (realVotes.length) {
            const ranks = realVotes.map((x) => SEV_RANK[x.severity]).sort((a, b) => a - b)
            const medRank = ranks[Math.floor((ranks.length - 1) / 2)]
            sev = Object.keys(SEV_RANK).find((k) => SEV_RANK[k] === medRank) || f.severity
          }
          return { ...f, severity: sev, verdict: { real, reasoning: v.map((x) => x.reasoning).join(' || '), votes: v.length, real_votes: realVotes.length } }
        })
    }
    return agent(verifyPrompt(f, SOLO_LENS), { label: 'verify:' + f.unit, phase: 'Verify', schema: VERDICT_SCHEMA })
      .then((v) => ({ ...f, severity: (v && v.real ? v.severity : f.severity), verdict: { real: !!(v && v.real), reasoning: v ? v.reasoning : 'verifier error', votes: 1, real_votes: v && v.real ? 1 : 0 } }))
  }),
)
const confirmed = verified.filter(Boolean).filter((f) => f.verdict && f.verdict.real)
confirmed.sort((a, b) => (SEV_RANK[a.severity] - SEV_RANK[b.severity]) || (CONF_RANK[a.confidence] - CONF_RANK[b.confidence]))
log(confirmed.length + ' of ' + toVerify.length + ' findings survived adversarial verification.')

// counts
const bySev = { critical: 0, high: 0, medium: 0, low: 0 }
const byDim = {}
confirmed.forEach((f) => { bySev[f.severity] = (bySev[f.severity] || 0) + 1; byDim[f.dimension] = (byDim[f.dimension] || 0) + 1 })

// ============================================================
phase('Synthesize')
const synthInput = {
  build: build || { builds: null, tests_pass: null, summary: 'build agent returned nothing', diagnostics: [] },
  counts: { by_severity: bySev, by_dimension: byDim, total: confirmed.length },
  findings: confirmed.map((f, i) => ({
    id: 'F' + (i + 1),
    title: f.title,
    file: f.file,
    line: f.line,
    dimension: f.dimension,
    severity: f.severity,
    confidence: f.confidence,
    description: f.description,
    suggested_fix: f.suggested_fix,
    verifier_note: f.verdict.reasoning,
  })),
}
const report = await agent(
  [
    'You are the lead reviewer writing the final report for an exhaustive audit of `diffo` (a Zig 0.16 TUI git-diff & code-review tool, ~9k LOC).',
    'Below is JSON with: the build/test result, summary counts, and the list of findings that already SURVIVED adversarial verification (each is considered real). Do not re-litigate whether they are real; your job is to organize, deduplicate near-duplicates, rank, and explain them clearly and actionably.',
    '',
    'Write a Markdown report with these sections:',
    '1. **Executive summary** — overall health in 3-5 sentences, the top 3-5 risks by name, and the severity/dimension counts.',
    '2. **Build & tests** — does it build, do tests pass, notable compiler diagnostics.',
    '3. **Findings** — grouped by severity (Critical -> High -> Medium -> Low). For each: a heading `**[F#] Title** — \\`file:line\\` _(dimension)_`, then 1-3 sentences on what is wrong and why it matters, then a **Fix:** line with the concrete remedy. Merge near-duplicate findings and say so.',
    '4. **Systemic patterns** — recurring root causes / themes across findings (e.g. ownership convention, FFI lifetime, unchecked casts).',
    '5. **Prioritized action list** — an ordered, concrete to-do list of the highest-leverage fixes.',
    '',
    'Be precise and engineering-focused. Keep every file:line reference. Output ONLY the Markdown report (no preamble).',
    '',
    'AUDIT DATA (JSON):',
    JSON.stringify(synthInput),
  ].join('\n'),
  { label: 'synthesize-report', phase: 'Synthesize' },
)

return {
  report,
  counts: synthInput.counts,
  build: { builds: build && build.builds, tests_pass: build && build.tests_pass, diagnostics: (build && build.diagnostics) || [] },
  confirmed_findings: synthInput.findings,
  stats: { raw: raw.length, deduped: deduped.length, verified: toVerify.length, confirmed: confirmed.length },
}
