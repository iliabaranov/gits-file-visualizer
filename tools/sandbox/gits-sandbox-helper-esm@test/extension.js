// Test-only helper for tools/sandbox-shell.sh on GNOME 45+: lets the harness call
// org.gnome.Shell.Eval and org.gnome.Shell.Screenshot in the sandbox shell.
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

export default class SandboxHelper extends Extension {
    enable() {
        global.context.unsafe_mode = true;
    }

    disable() {
        global.context.unsafe_mode = false;
    }
}
