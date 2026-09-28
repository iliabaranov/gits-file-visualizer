// Syntax-checks extension JS files with the same engine GNOME Shell uses:
//   gjs tools/check-extension.js extension/gnome-42/extension.js
const { GLib } = imports.gi;
const ByteArray = imports.byteArray;
let failed = false;
for (const path of ARGV) {
    const [ok, bytes] = GLib.file_get_contents(path);
    try {
        new Function(ByteArray.toString(bytes));
        print(`ok      ${path}`);
    } catch (e) {
        failed = true;
        print(`ERROR   ${path}: ${e}`);
    }
}
if (failed)
    imports.system.exit(1);
