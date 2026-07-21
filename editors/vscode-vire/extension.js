// Vire VS Code extension.
//
// Language intelligence (diagnostics, hover, go-to-definition, completion, quick
// fixes) comes from the Vire frontend's JSON analysis `{ diagnostics, symbols }`.
// It runs the native `vire check --json` when a `vire` binary is available
// (fast, reliable), and otherwise falls back to the BUNDLED WebAssembly frontend
// (wasm/vire-check.wasm) via Node's WASI — so it also works on any machine with
// no toolchain. The native `vire` is additionally needed for Build/Run/Debug.
'use strict';
const vscode = require('vscode');
const cp = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');

function virePath() {
    return vscode.workspace.getConfiguration('vire').get('path', 'vire');
}

// ----------------------------- analysis backends -----------------------

let wasmModule = null;
let WASI = null;
let nativeBroken = false;   // set once native `vire` proves unavailable
let extPath = '';

function loadWasm(context) {
    extPath = context.extensionPath;
    try {
        WASI = require('node:wasi').WASI;
        wasmModule = new WebAssembly.Module(fs.readFileSync(path.join(extPath, 'wasm', 'vire-check.wasm')));
    } catch (e) {
        console.error('Vire: bundled wasm frontend unavailable:', e && e.message);
    }
}

// Native `vire check --json <tmpfile>` over the current (unsaved) buffer.
function analyzeNative(src) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'vire-chk-'));
    const f = path.join(dir, 'buf.vr');
    try {
        fs.writeFileSync(f, src);
        const out = cp.execFileSync(virePath(), ['check', '--json', f], { encoding: 'utf8', timeout: 15000, stdio: ['ignore', 'pipe', 'pipe'] });
        return JSON.parse(out.trim());
    } catch (e) {
        // execFileSync throws on non-zero exit too, but check --json always exits 0;
        // a throw here means the binary is missing/not runnable → fall back to wasm.
        if (e && (e.code === 'ENOENT' || e.code === 'EACCES')) nativeBroken = true;
        else if (e && e.stdout) { try { return JSON.parse(e.stdout.toString().trim()); } catch (_) { } }
        return null;
    } finally {
        try { fs.rmSync(dir, { recursive: true, force: true }); } catch (_) { }
    }
}

function analyzeWasm(src, name) {
    if (!wasmModule || !WASI) return null;
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'vire-wasm-'));
    const inF = path.join(dir, 'in.vr'), outF = path.join(dir, 'out.json');
    try {
        fs.writeFileSync(inF, src);
        fs.writeFileSync(outF, '');
        const stdin = fs.openSync(inF, 'r'), stdout = fs.openSync(outF, 'w');
        const wasi = new WASI({ version: 'preview1', args: ['vire-check', name || 'file.vr', '--json'], stdin, stdout, returnOnExit: true });
        wasi.start(new WebAssembly.Instance(wasmModule, wasi.getImportObject()));
        fs.closeSync(stdin); fs.closeSync(stdout);
        const txt = fs.readFileSync(outF, 'utf8').trim();
        return txt ? JSON.parse(txt) : { diagnostics: [], symbols: [] };
    } catch (e) {
        console.error('Vire: wasm analyze failed:', e && e.message);
        return null;
    } finally {
        try { fs.rmSync(dir, { recursive: true, force: true }); } catch (_) { }
    }
}

function analyze(src, name) {
    if (!nativeBroken) {
        const r = analyzeNative(src);
        if (r) return r;
    }
    return analyzeWasm(src, name);
}

// ----------------------------- diagnostics -----------------------------

let diagnostics;
const docSymbols = new Map();   // uri -> [{name, kind, line, col, signature}]
const docTypes = new Map();     // uri -> [{sl, sc, el, ec, type}] (1-based, end exclusive)
const debounceTimers = new Map();

function refresh(doc) {
    if (doc.languageId !== 'vire') return;
    const res = analyze(doc.getText(), path.basename(doc.fileName));
    if (!res) return;
    docSymbols.set(doc.uri.toString(), res.symbols || []);
    docTypes.set(doc.uri.toString(), res.types || []);
    const items = (res.diagnostics || []).map(d => {
        const l = Math.max(0, d.line - 1), c = Math.max(0, d.col - 1);
        const sev = d.severity === 'warning' ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error;
        const diag = new vscode.Diagnostic(new vscode.Range(l, c, l, c + 1), d.message, sev);
        diag.source = 'vire';
        return diag;
    });
    diagnostics.set(doc.uri, items);
}

function scheduleRefresh(doc, delay) {
    if (doc.languageId !== 'vire') return;
    const key = doc.uri.toString();
    clearTimeout(debounceTimers.get(key));
    debounceTimers.set(key, setTimeout(() => refresh(doc), delay));
}

// ------------------------- hover / definition --------------------------

function symbolAt(document, position) {
    const range = document.getWordRangeAtPosition(position, /[A-Za-z_][A-Za-z0-9_]*/);
    if (!range) return null;
    const word = document.getText(range);
    return (docSymbols.get(document.uri.toString()) || []).find(s => s.name === word) || null;
}

// Smallest inferred-type range covering `position` (0-based). Entries are
// 1-based with an exclusive end.
function typeAt(document, position) {
    const types = docTypes.get(document.uri.toString()) || [];
    const L = position.line + 1, C = position.character + 1;
    let best = null, bestSize = Infinity;
    for (const t of types) {
        const afterStart = L > t.sl || (L === t.sl && C >= t.sc);
        const beforeEnd = L < t.el || (L === t.el && C < t.ec);
        if (afterStart && beforeEnd) {
            const size = (t.el - t.sl) * 1e6 + (t.ec - t.sc);
            if (size < bestSize) { bestSize = size; best = t; }
        }
    }
    return best;
}

const hoverProvider = {
    provideHover(document, position) {
        // A named definition (function/type/…) wins — show its signature.
        const s = symbolAt(document, position);
        if (s) {
            const md = new vscode.MarkdownString();
            md.appendCodeblock(s.signature, 'vire');
            return new vscode.Hover(md);
        }
        // Otherwise show the inferred type of the expression under the cursor.
        const t = typeAt(document, position);
        if (t) {
            const range = document.getWordRangeAtPosition(position, /[A-Za-z_][A-Za-z0-9_]*/);
            const md = new vscode.MarkdownString();
            md.appendCodeblock(`${document.getText(range) || 'expr'}: ${t.type}`, 'vire');
            return new vscode.Hover(md, range);
        }
        return null;
    }
};

const definitionProvider = {
    provideDefinition(document, position) {
        const s = symbolAt(document, position);
        if (!s) return null;
        return new vscode.Location(document.uri, new vscode.Position(Math.max(0, s.line - 1), Math.max(0, s.col - 1)));
    }
};

const symbolProvider = {
    provideDocumentSymbols(document) {
        const kind = k => k === 'type' ? vscode.SymbolKind.Struct : k === 'trait' ? vscode.SymbolKind.Interface : k === 'const' ? vscode.SymbolKind.Constant : vscode.SymbolKind.Function;
        return (docSymbols.get(document.uri.toString()) || []).map(s => {
            const pos = new vscode.Position(Math.max(0, s.line - 1), Math.max(0, s.col - 1));
            return new vscode.SymbolInformation(s.name, kind(s.kind), '', new vscode.Location(document.uri, new vscode.Range(pos, pos)));
        });
    }
};

// ------------------------------ completion -----------------------------

const KEYWORDS = ['fn', 'type', 'trait', 'impl', 'mut', 'const', 'use', 'pub', 'extern', 'unsafe', 'macro', 'comptime',
    'match', 'if', 'elif', 'else', 'while', 'for', 'in', 'break', 'continue', 'return', 'spawn', 'capsule', 'native',
    'and', 'or', 'not', 'as', 'true', 'false', 'self', 'Self'];
const TYPES = ['Int', 'Float', 'Bool', 'Str', 'Void', 'Array', 'Map', 'List', 'Set', 'Option', 'Result',
    'Atomic', 'Mutex', 'Channel', 'array', 'farray', 'list', 'map', 'set', 'F32', 'F64', 'I32', 'I64'];
const BUILTINS = [
    ['print', 'print(value) — write a value + newline'],
    ['array', 'array(n) — new Int array of length n'],
    ['farray', 'farray(n) — new Float array of length n'],
    ['list', 'list() — growable list'],
    ['map', 'map() — hash map'],
    ['set', 'set() — hash set'],
    ['parallel_for', 'parallel_for(n, shared, worker) — fork/join over 0..n'],
    ['gpu_gid', 'gpu_gid() — global GPU thread index (inside @gpu)'],
    ['gpu_gsize', 'gpu_gsize() — total GPU thread count (grid stride)'],
    ['gpu_tid', 'gpu_tid() — threadIdx.x'], ['gpu_bid', 'gpu_bid() — blockIdx.x'],
    ['gpu_bdim', 'gpu_bdim() — blockDim.x'], ['gpu_gdim', 'gpu_gdim() — gridDim.x'],
];

// Methods offered after `.` — the runtime methods on the built-in collections
// and strings (the receiver's exact type isn't tracked, so offer the union).
const METHODS = [
    ['len', 'length'], ['push', 'list: append'], ['pop', 'list: remove last'],
    ['get', 'list/map: element'], ['set', 'list: assign'], ['contains', 'membership'],
    ['clear', 'empty the collection'], ['put', 'map: insert'], ['has', 'map: key present'],
    ['remove', 'map/set: delete'], ['add', 'set: insert'],
    ['charAt', 'str: char at index'], ['indexOf', 'str: find'], ['substring', 'str: slice'],
    ['upper', 'str: uppercase'], ['lower', 'str: lowercase'], ['trim', 'str: strip'],
    ['startsWith', 'str: prefix?'], ['endsWith', 'str: suffix?'], ['equals', 'str: equality'],
    ['fetch_add', 'Atomic: add + return old'], ['load', 'Atomic: read'],
    ['lock', 'Mutex'], ['unlock', 'Mutex'], ['send', 'Channel'], ['recv', 'Channel'],
];

const completionProvider = {
    provideCompletionItems(document, position) {
        const CI = vscode.CompletionItemKind;
        const prefix = document.lineAt(position.line).text.slice(0, position.character);

        // After `.` → method completion.
        if (/\.[A-Za-z0-9_]*$/.test(prefix)) {
            return METHODS.map(([n, doc]) => {
                const it = new vscode.CompletionItem(n, CI.Method);
                it.detail = doc;
                return it;
            });
        }
        // Type position: after `:` (annotation) or `->` (return type) → types only.
        if (/(:|->)\s*[A-Za-z0-9_]*$/.test(prefix)) {
            const out = TYPES.map(t => new vscode.CompletionItem(t, CI.Class));
            for (const s of docSymbols.get(document.uri.toString()) || []) {
                if (s.kind === 'type' || s.kind === 'trait') out.push(new vscode.CompletionItem(s.name, CI.Struct));
            }
            return out;
        }
        // Default: keywords + builtins + this file's definitions.
        const out = [];
        for (const k of KEYWORDS) out.push(new vscode.CompletionItem(k, CI.Keyword));
        for (const t of TYPES) out.push(new vscode.CompletionItem(t, CI.Class));
        for (const [n, doc] of BUILTINS) {
            const it = new vscode.CompletionItem(n, CI.Function);
            it.detail = doc;
            out.push(it);
        }
        for (const s of docSymbols.get(document.uri.toString()) || []) {
            const kind = s.kind === 'type' ? CI.Struct : s.kind === 'trait' ? CI.Interface : s.kind === 'const' ? CI.Constant : CI.Function;
            const it = new vscode.CompletionItem(s.name, kind);
            it.detail = s.signature;
            out.push(it);
        }
        return out;
    }
};

// ---------------------- quick fixes (autocorrect) ----------------------

function levenshtein(a, b) {
    const m = a.length, n = b.length;
    const d = Array.from({ length: m + 1 }, (_, i) => [i, ...Array(n).fill(0)]);
    for (let j = 0; j <= n; j++) d[0][j] = j;
    for (let i = 1; i <= m; i++) for (let j = 1; j <= n; j++)
        d[i][j] = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
    return d[m][n];
}

// Candidate names known in the file: its symbols + parameters/locals in scope +
// the builtins/types. Used to propose "did you mean X?" corrections.
function knownNames(document) {
    const names = new Set([...TYPES, ...BUILTINS.map(b => b[0])]);
    for (const s of docSymbols.get(document.uri.toString()) || []) names.add(s.name);
    const text = document.getText();
    let m;
    const re = /\b(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\b/g;
    while ((m = re.exec(text))) names.add(m[1]);
    return [...names];
}

const codeActionProvider = {
    provideCodeActions(document, range, context) {
        const actions = [];
        for (const diag of context.diagnostics) {
            if (diag.source !== 'vire') continue;
            // "unknown variable: X" / "X has no method Y" / "... `X` ..." → suggest a close name.
            const m = /unknown (?:variable|function|type)[:]?\s*`?([A-Za-z_][A-Za-z0-9_]*)`?/.exec(diag.message)
                || /`([A-Za-z_][A-Za-z0-9_]*)`/.exec(diag.message);
            if (!m) continue;
            const bad = m[1];
            const cands = knownNames(document)
                .filter(n => n !== bad)
                .map(n => [n, levenshtein(bad.toLowerCase(), n.toLowerCase())])
                .filter(([, d]) => d <= Math.max(1, Math.floor(bad.length / 3)))
                .sort((a, b) => a[1] - b[1])
                .slice(0, 3);
            // Locate the bad identifier on the diagnostic's line.
            const lineText = document.lineAt(diag.range.start.line).text;
            const idx = lineText.indexOf(bad);
            const wordRange = idx >= 0
                ? new vscode.Range(diag.range.start.line, idx, diag.range.start.line, idx + bad.length)
                : diag.range;
            for (const [name] of cands) {
                const fix = new vscode.CodeAction(`Change to \`${name}\``, vscode.CodeActionKind.QuickFix);
                fix.edit = new vscode.WorkspaceEdit();
                fix.edit.replace(document.uri, wordRange, name);
                fix.diagnostics = [diag];
                fix.isPreferred = cands.length > 0 && name === cands[0][0];
                actions.push(fix);
            }
        }
        return actions;
    }
};

// ------------------------------- commands ------------------------------

function runInTerminal(name, args) {
    const term = vscode.window.terminals.find(t => t.name === name) || vscode.window.createTerminal(name);
    term.show(true);
    term.sendText(`${virePath()} ${args.map(a => (/\s/.test(a) ? `"${a}"` : a)).join(' ')}`);
}

function currentFile() {
    const ed = vscode.window.activeTextEditor;
    if (!ed || ed.document.languageId !== 'vire') {
        vscode.window.showErrorMessage('Vire: no active .vr file.');
        return undefined;
    }
    return ed.document.fileName;
}

// ------------------------------- debugging -----------------------------

class VireDebugConfigProvider {
    resolveDebugConfiguration(folder, config) {
        if (!config.type) {
            const f = currentFile();
            if (!f) return undefined;
            config = { type: 'vire', request: 'launch', name: 'Vire: Debug current file', program: f };
        }
        const src = config.program;
        if (!src || !src.endsWith('.vr')) {
            vscode.window.showErrorMessage('Vire debug: `program` must be a .vr source file.');
            return undefined;
        }
        const out = path.join(os.tmpdir(), 'vire-dbg-' + path.basename(src, '.vr') + '-' + process.pid);
        try {
            cp.execFileSync(virePath(), ['build', '--debug', src, '-o', out], { stdio: 'pipe' });
        } catch (e) {
            vscode.window.showErrorMessage('Vire debug: compilation failed:\n' + ((e.stderr ? e.stderr.toString() : '') || e.message));
            return undefined;
        }
        config.program = out;
        if (!config.cwd && folder) config.cwd = folder.uri.fsPath;
        return config;
    }
}

class VireDebugAdapterFactory {
    createDebugAdapterDescriptor() {
        return new vscode.DebugAdapterExecutable('lldb-dap', []);
    }
}

// ------------------------------- activate ------------------------------

function activate(context) {
    diagnostics = vscode.languages.createDiagnosticCollection('vire');
    context.subscriptions.push(diagnostics);
    loadWasm(context);

    context.subscriptions.push(
        vscode.workspace.onDidOpenTextDocument(d => refresh(d)),
        vscode.workspace.onDidSaveTextDocument(d => refresh(d)),
        vscode.workspace.onDidChangeTextDocument(e => scheduleRefresh(e.document, 300)),
        vscode.workspace.onDidCloseTextDocument(d => { diagnostics.delete(d.uri); docSymbols.delete(d.uri.toString()); docTypes.delete(d.uri.toString()); }),
    );
    // Analyze all already-open .vr docs on activation.
    for (const d of vscode.workspace.textDocuments) refresh(d);

    context.subscriptions.push(
        vscode.languages.registerHoverProvider('vire', hoverProvider),
        vscode.languages.registerDefinitionProvider('vire', definitionProvider),
        vscode.languages.registerDocumentSymbolProvider('vire', symbolProvider),
        vscode.languages.registerCompletionItemProvider('vire', completionProvider, '.'),
        vscode.languages.registerCodeActionsProvider('vire', codeActionProvider, { providedCodeActionKinds: [vscode.CodeActionKind.QuickFix] }),
    );

    context.subscriptions.push(
        vscode.commands.registerCommand('vire.build', () => {
            const f = currentFile();
            if (f) runInTerminal('Vire', ['build', f, '-o', path.join(path.dirname(f), path.basename(f, '.vr'))]);
        }),
        vscode.commands.registerCommand('vire.run', () => {
            const f = currentFile();
            if (f) runInTerminal('Vire', ['run', f]);
        }),
    );

    context.subscriptions.push(
        vscode.debug.registerDebugConfigurationProvider('vire', new VireDebugConfigProvider()),
        vscode.debug.registerDebugAdapterDescriptorFactory('vire', new VireDebugAdapterFactory()),
    );
}

function deactivate() { }

module.exports = { activate, deactivate };
