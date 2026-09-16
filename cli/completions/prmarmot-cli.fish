# fish completion for prmarmot-cli
#
# Install:
#   prmarmot-cli completions fish > ~/.config/fish/completions/prmarmot-cli.fish

set -l commands mine authored review reviews watch skill completions help
set -l boards mine authored review reviews
set -l views $boards watch

# `--until` takes a comma-separated list: offer each condition after the
# ones already typed.
function __prmarmot_cli_conditions
    set -l token (string replace -r -- '^--until=' '' (commandline -ct))
    set -l typed (string match -r -- '^.*,' $token)
    for condition in ci-pass approved mergeable merged
        printf '%s%s\n' "$typed" $condition
    end
end

# True for `watch` before any other argument: where `mine` or `review` may go.
function __prmarmot_cli_watch_view_expected
    set -l words (commandline -opc)
    test (count $words) -eq 2; and test "$words[2]" = watch
end

complete -c prmarmot-cli -f

# Commands
complete -c prmarmot-cli -n "not __fish_seen_subcommand_from $commands" -s V -l version -d 'Show the version'
complete -c prmarmot-cli -n "not __fish_seen_subcommand_from $commands" -a mine -d 'PRs you authored (My PRs)'
complete -c prmarmot-cli -n "not __fish_seen_subcommand_from $commands" -a review -d 'PRs waiting for your review'
complete -c prmarmot-cli -n "not __fish_seen_subcommand_from $commands" -a watch -d 'Print what changes, one event per line'
complete -c prmarmot-cli -n "not __fish_seen_subcommand_from $commands" -a skill -d 'Print or install the coding-agent skill'
complete -c prmarmot-cli -n "not __fish_seen_subcommand_from $commands" -a completions -d 'Print a shell completion script'
complete -c prmarmot-cli -n "not __fish_seen_subcommand_from $commands" -a help -d 'Show help'
complete -c prmarmot-cli -s h -l help -d 'Show help'

# mine, review, and watch
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $views" -l repo -x -d 'One repository (OWNER/NAME)'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $views" -l all-repos -d 'Every repository involving you'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from mine authored watch; and not __fish_seen_subcommand_from review reviews" -l authored -d 'With --all-repos: only PRs you authored'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $views" -l json -d 'JSON output'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $views" -l watched -d 'Only PRs you watch in PR Marmot'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $views" -l snoozed -d 'Include snoozed PRs'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $views" -l no-color -d 'Plain text'

# mine and review
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $boards; and not __fish_seen_subcommand_from watch" -s f -l format -x -a 'table markdown json' -d 'Output format'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $boards; and not __fish_seen_subcommand_from watch" -l changed -d 'Only PRs changed since you last looked'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $boards; and not __fish_seen_subcommand_from watch" -l stale -d 'Only PRs that have waited too long for a reviewer'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from review reviews; and not __fish_seen_subcommand_from watch" -l sort -x -a 'wait smallest' -d 'Longest wait or smallest change first'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from $boards; and not __fish_seen_subcommand_from watch" -l pages -x -a '1 2 3 4 5' -d 'Result pages to load per queue'

# watch
complete -c prmarmot-cli -n __prmarmot_cli_watch_view_expected -a 'mine review' -d 'View to watch'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from watch" -s f -l format -x -a 'text json' -d 'Output format'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from watch" -l interval -x -d 'Poll interval in seconds, at least 30'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from watch" -l events -x -d 'Exit after N change events'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from watch; and not __fish_seen_subcommand_from review reviews" -l pr -x -d 'One pull request (OWNER/NAME#N or URL)'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from watch; and not __fish_seen_subcommand_from review reviews" -l until -x -a '(__prmarmot_cli_conditions)' -d 'Stop once the PR reaches a condition'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from watch; and not __fish_seen_subcommand_from review reviews" -l timeout -x -a '90s 5m 30m 1h 2h' -d 'Give up after a duration'

# skill
complete -c prmarmot-cli -n "__fish_seen_subcommand_from skill; and not __fish_seen_subcommand_from install" -a install -d 'Install as a user-level skill'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from install" -l agent -x -a 'claude agents all' -d 'Which agents read the skill'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from install" -l dir -x -a '(__fish_complete_directories)' -d 'Any other skills directory'
complete -c prmarmot-cli -n "__fish_seen_subcommand_from install" -l force -d 'Replace a copy that differs or is a symlink'

# completions
complete -c prmarmot-cli -n "__fish_seen_subcommand_from completions; and not __fish_seen_subcommand_from bash zsh fish" -a 'bash zsh fish' -d Shell
