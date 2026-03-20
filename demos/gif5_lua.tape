Output demos/gif5_lua.gif
Set FontSize 16
Set Width 1200
Set Height 700
Set Theme "Dracula"
Set Shell "zsh"
Set TypingSpeed 30ms

Hide
Type "export PATH=/Users/nik/github/project-alpha/.claude/worktrees/agent-ae56767d/.claude/worktrees/agent-a07c24b0/target/debug:$PATH"
Enter
Sleep 500ms
Type "clear"
Enter
Sleep 500ms
Show

Type "rpg -d demo_saas"
Sleep 500ms
Enter
Sleep 2s

# Show all registered custom commands
Type "\commands"
Enter
Sleep 2s

Enter
Sleep 300ms

# Run slow_mean — top queries by average execution time
Type "\slow_mean"
Enter
Sleep 3s

Enter
Sleep 300ms

# Run slow_total — top queries by total execution time
Type "\slow_total"
Enter
Sleep 3s

Enter
Sleep 300ms

# Run table_info for the orders table
Type "\table_info orders"
Enter
Sleep 3s

Sleep 2s
