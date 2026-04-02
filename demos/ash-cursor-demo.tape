Output demos/ash-cursor-demo.gif
Set FontSize 15
Set Width 1400
Set Height 800
Set Theme "Dracula"
Set Shell "zsh"
Set TypingSpeed 30ms

# --- Hidden setup: start workload + set PATH ---
Hide
Type "export PATH=/Users/nik/github/rpg/target/debug:$PATH"
Enter
Sleep 500ms
Type "bash /Users/nik/github/rpg/demos/ash-cursor-workload.sh &"
Enter
Sleep 500ms
Type "clear"
Enter
Sleep 500ms
Show

# --- Connect ---
Type "rpg -d demo_saas"
Sleep 300ms
Enter
Sleep 2s

# --- Launch /ash ---
Type "/ash"
Enter
Sleep 8s

# --- Zoom out to 15s buckets ---
Type "]"
Sleep 3s

# --- Zoom back to 1s ---
Type "["
Sleep 3s

# --- Pan left into history (cursor appears) ---
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 2s

# --- Pan right back towards present ---
Right
Sleep 1000ms
Right
Sleep 1000ms
Right
Sleep 1000ms
Right
Sleep 1000ms
Right
Sleep 2s

# --- Drill down into a wait type ---
Enter
Sleep 2s

# --- Pan left in drilled view (context-sensitive timeline) ---
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 1500ms
Left
Sleep 2s

# --- Esc back to live ---
Escape
Sleep 3s

# --- Back to top level ---
Type "b"
Sleep 2s

# --- Esc to quit /ash ---
Escape
Sleep 1s

# --- Clean exit ---
Type "\q"
Enter
Sleep 1s

# --- Hidden cleanup: kill workload ---
Hide
Type "kill %1 2>/dev/null; wait 2>/dev/null"
Enter
Sleep 500ms
