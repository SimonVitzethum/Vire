// Vire VS Code extension.
//
// Language intelligence (diagnostics, hover, go-to-definition) runs the bundled
// **WebAssembly** build of the Vire frontend (wasm/vire-check.wasm) via Node's
// built-in WASI — so it works on Windows/macOS/Linux with NO native toolchain.
// The native `vire` compiler is only needed for Build/Run and debugging (which
// require clang/lldb anyway).
'use strict';
const vscode = require('vscode');
const cp = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');

function virePath() {
    return vscode.workspace.getConfiguration('vire').get('path', 'vire');
}

// ----------------------------- wasm frontend ---------------------------

let wasmModule = null; // compiled WebAssembly.Module (cached)
let WASI = null;

function loadWasm(context) {
    try {
        WASI = require('node:wasi').WASI;
        const bytes = fs.readFileSync(path.join(context.extensionPath, 'wasm', 'vire-check.wasm'));
        wasmModule = new WebAssembly.Module(bytes);
        return true;
    } catch (e) {
        console.error('Vire: failed to load bundled wasm frontend:', e);
        return false;
    }
}

// Run the wasm frontend over `src`; returns { diagnostics, symbols } or null.
function analyze(src, name) {
    if (!wasmModule || !WASI) return null;
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'vire-wasm-'));
    const inF = path.join(dir, 'in.vr');
    const outF = path.join(dir, 'out.json');
    try {
        fs.writeFileSync(inF, src);
        fs.writeFileSync(outF, '');
        const stdin = fs.openSync(inF, 'r');
        const stdout = fs.openSync(outF, 'w');
        const wasi = new WASI({ version: 'preview1', args: ['vire-check', name || 'file.vr', '--json'], stdin, stdout, returnOnExit: true });
        const inst = new WebAssembly.Instance(wasmModule, wasi.getImportObject());
        wasi.start(inst);
        fs.closeSync(stdin);
        fs.closeSync(stdout);
        const txt = fs.readFileSync(outF, 'utf8').trim();
        return txt ? JSON.parse(txt) : { diagnostics: [], symbols: [] };
    } catch (e) {
        console.error('Vire: wasm analyze failed:', e);
        return null;
    } finally {
        try { fs.rmSync(dir, { recursive: true, force: true }); } catch (_) { }
    }
}

// ----------------------------- diagnostics -----------------------------

let diagnostics;
const docSymbols = new Map();   // uri.toString() -> [{name, kind, line, col, signature}]
const debounceTimers = new Map();

function refresh(doc) {
    if (doc.languageId !== 'vire') return;
    const res = analyze(doc.getText(), path.basename(doc.fileName));
    if (!res) return;
    docSymbols.set(doc.uri.toString(), res.symbols || []);
    if (vscode.workspace.getConfiguration('vire').get('checkOnSave', true)) {
        const items = (res.diagnostics || []).map(d => {
            const l = Math.max(0, d.line - 1), c = Math.max(0, d.col - 1);
            const sev = d.severity === 'warning' ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error;
            return new vscode.Diagnostic(new vscode.Range(l, c, l, c + 1), d.message, sev);
        });
        diagnostics.set(doc.uri, items);
    } else {
        diagnostics.set(doc.uri, []);
    }
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
    const syms = docSymbols.get(document.uri.toString()) || [];
    return syms.find(s => s.name === word) || null;
}

const hoverProvider = {
    provideHover(document, position) {
        const s = symbolAt(document, position);
        if (!s) return null;
        const md = new vscode.MarkdownString();
        md.appendCodeblock(s.signature, 'vire');
        return new vscode.Hover(md);
    }
};

const definitionProvider = {
    provideDefinition(document, position) {
        const s = symbolAt(document, position);
        if (!s) return null;
        const pos = new vscode.Position(Math.max(0, s.line - 1), Math.max(0, s.col - 1));
        return new vscode.Location(document.uri, pos);
    }
};

const symbolProvider = {
    provideDocumentSymbols(document) {
        const syms = docSymbols.get(document.uri.toString()) || [];
        const kind = k => k === 'type' ? vscode.SymbolKind.Struct
            : k === 'trait' ? vscode.SymbolKind.Interface
                : k === 'const' ? vscode.SymbolKind.Constant
                    : vscode.SymbolKind.Function;
        return syms.map(s => {
            const pos = new vscode.Position(Math.max(0, s.line - 1), Math.max(0, s.col - 1));
            const range = new vscode.Range(pos, pos);
            return new vscode.SymbolInformation(s.name, kind(s.kind), '', new vscode.Location(document.uri, range));
        });
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

    const haveWasm = loadWasm(context);
    if (!haveWasm) {
        vscode.window.showWarningMessage('Vire: bundled wasm frontend missing — diagnostics/hover disabled. (Build it with editors/vscode-vire/build-wasm.sh.)');
    }

    context.subscriptions.push(
        vscode.workspace.onDidOpenTextDocument(d => refresh(d)),
        vscode.workspace.onDidSaveTextDocument(d => refresh(d)),
        vscode.workspace.onDidChangeTextDocument(e => scheduleRefresh(e.document, 350)),
        vscode.workspace.onDidCloseTextDocument(d => { diagnostics.delete(d.uri); docSymbols.delete(d.uri.toString()); }),
    );
    if (vscode.window.activeTextEditor) refresh(vscode.window.activeTextEditor.document);

    context.subscriptions.push(
        vscode.languages.registerHoverProvider('vire', hoverProvider),
        vscode.languages.registerDefinitionProvider('vire', definitionProvider),
        vscode.languages.registerDocumentSymbolProvider('vire', symbolProvider),
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
