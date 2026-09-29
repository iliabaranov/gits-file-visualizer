// Syntax-checks extension JS files with the same engine GNOME Shell uses:
//   gjs tools/check-extension.js extension/gnome-42/extension.js extension/gnome-45/extension.js
// ES-module files (GNOME 45+) are checked with their import/export syntax removed,
// since gjs cannot resolve GNOME Shell's resource:// modules outside the shell.
const { GLib } = imports.gi;
const ByteArray = imports.byteArray;
let failed = false;
for (const path of ARGV) {
    const [ok, bytes] = GLib.file_get_contents(path);
    let text = ByteArray.toString(bytes);
    if (/^import\s/m.test(text) || /^export\s/m.test(text))
        text = text.replace(/^import\s.*$/gm, '').replace(/^export default class /gm, 'class ');
    try {
        new Function(text);
        print(`ok      ${path}`);
    } catch (e) {
        failed = true;
        print(`ERROR   ${path}: ${e}`);
    }
}
if (failed)
    imports.system.exit(1);
