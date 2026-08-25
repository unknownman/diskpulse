# diskpulse demo — rendered with VHS (https://github.com/charmbracelet/vhs)
# SAFETY: every command below operates ONLY inside /tmp/diskpulse-demo, a
# synthetic fixture built by demo/build_fixture.sh. `clean` appears in
# DRY-RUN mode only (--apply is never used).

Output demo/diskpulse-demo.gif

Set Shell bash
Set FontSize 17
Set Width 1250
Set Height 740
Set Theme { "background": "#1e1e2e", "foreground": "#cdd6f4", "cursor": "#f5e0dc" }
Set TypingSpeed 50ms

Type "cd /tmp/diskpulse-demo"
Enter
Sleep 1s

# Default tree view: bars, percentages, dev dirs ignored
Type "diskpulse viz demo-project"
Enter
Sleep 4s

# Deeper traversal with top-N truncation and the aggregate pruned row
Type "diskpulse viz demo-project --depth 3 --top 5"
Enter
Sleep 4s

# Contrast: include the heavyweight dev directories
Type "diskpulse viz demo-project --no-ignore"
Enter
Sleep 4s

# Alternate sort mode
Type "diskpulse viz demo-project --sort count"
Enter
Sleep 3s

# Glob excludes
Type 'diskpulse viz demo-project --exclude "*.log"'
Enter
Sleep 3s

# JSON output contract for scripting
Type "diskpulse viz demo-project --json | head -20"
Enter
Sleep 2.5s

# Cache cleaning preview — DRY RUN ONLY, nothing is deleted
Type "diskpulse clean --path demo-cache"
Enter
Sleep 4.5s

# The registered target catalog (read-only metadata)
Type "diskpulse targets"
Enter
Sleep 3s

Sleep 2.5s
