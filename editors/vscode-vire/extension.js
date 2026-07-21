// Vire VS Code extension: diagnostics (`vire check`), build/run commands, and
// native debugging (compile with `--debug`, then hand off to lldb-dap).
'use strict';
const vscode = require('vscode');
const cp = require('child_process');
const path = require('path');
const os = require('os');

/** Resolve the configured `vire` binary path. */
function virePath() {
    return vscode.workspace.getConfiguration('vire').get('path', 'vire');
}

// ----------------------------- diagnostics -----------------------------

let diagnostics;

/** Run `vire check <file>` and turn its `file:line:col: severity: msg` lines
 *  into VS Code diagnostics. Location-less compiler errors land at 1:1. */
function checkDocument(doc) {
    if (doc.languageId !== 'vire') return;
    if (!vscode.workspace.getConfiguration('vire').get('checkOnSave', true)) return;
    const file = doc.fileName;
    cp.execFile(virePath(), ['check', file], { timeout: 20000 }, (err, stdout, stderr) => {
        const out = `${stdout || ''}\n${stderr || ''}`;
        const items = [];
        // `path:line:col: severity: message`
        const re = /^(.*?):(\d+):(\d+):\s*(error|warning|note)?:?\s*(.*)$/;
        for (const line of out.split('\n')) {
            const m = re.exec(line.trim());
            if (!m) continue;
            const l = Math.max(0, parseInt(m[2], 10) - 1);
            const c = Math.max(0, parseInt(m[3], 10) - 1);
            const sev = m[4] === 'warning' ? vscode.DiagnosticSeverity.Warning
                : m[4] === 'note' ? vscode.DiagnosticSeverity.Information
                    : vscode.DiagnosticSeverity.Error;
            const range = new vscode.Range(l, c, l, c + 1);
            items.push(new vscode.Diagnostic(range, m[5] || line.trim(), sev));
        }
        diagnostics.set(doc.uri, items);
    });
}

// ------------------------------- commands ------------------------------

function runInTerminal(name, args) {
    const term = vscode.window.terminals.find(t => t.name === name) || vscode.window.createTerminal(name);
    term.show(true);
    const quoted = args.map(a => (/\s/.test(a) ? `"${a}"` : a)).join(' ');
    term.sendText(`${virePath()} ${quoted}`);
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

// Compile the .vr with `--debug` and rewrite `program` to the produced binary,
// then let lldb-dap (the debug adapter) launch it. lldb-dap understands the
// `program`/`args`/`cwd`/`stopOnEntry` launch attributes directly.
class VireDebugConfigProvider {
    resolveDebugConfiguration(folder, config) {
        if (!config.type) {
            // No launch.json: debug the active file.
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
            const msg = (e.stderr ? e.stderr.toString() : '') || e.message;
            vscode.window.showErrorMessage('Vire debug: compilation failed:\n' + msg);
            return undefined;
        }
        // Hand off to lldb-dap with the compiled native binary.
        config.program = out;
        if (!config.cwd && folder) config.cwd = folder.uri.fsPath;
        return config;
    }
}

class VireDebugAdapterFactory {
    createDebugAdapterDescriptor() {
        // Use the system lldb-dap (ships with LLVM/lldb) as the DAP backend.
        return new vscode.DebugAdapterExecutable('lldb-dap', []);
    }
}

// ------------------------------- activate ------------------------------

function activate(context) {
    diagnostics = vscode.languages.createDiagnosticCollection('vire');
    context.subscriptions.push(diagnostics);

    context.subscriptions.push(
        vscode.workspace.onDidSaveTextDocument(checkDocument),
        vscode.workspace.onDidOpenTextDocument(checkDocument),
    );
    if (vscode.window.activeTextEditor) checkDocument(vscode.window.activeTextEditor.document);

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
