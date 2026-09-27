// Test-only helper for tools/sandbox-shell.sh: lets the harness call
// org.gnome.Shell.Eval and org.gnome.Shell.Screenshot in the sandbox shell.
/* exported init */
function init() {
    return {
        enable() {
            global.context.unsafe_mode = true;
        },
        disable() {
            global.context.unsafe_mode = false;
        },
    };
}
