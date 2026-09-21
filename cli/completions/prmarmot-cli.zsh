#compdef prmarmot-cli
# zsh completion for prmarmot-cli
#
# Install into a directory on $fpath, before compinit runs in ~/.zshrc:
#   mkdir -p ~/.zfunc && prmarmot-cli completions zsh > ~/.zfunc/_prmarmot-cli
#   # ~/.zshrc: fpath=(~/.zfunc $fpath); autoload -Uz compinit && compinit
# or load it after compinit:
#   source <(prmarmot-cli completions zsh)

_prmarmot-cli() {
  local curcontext=$curcontext state line ret=1
  typeset -A opt_args

  local -a help scope view watch
  help=('(- *)'{-h,--help}'[show help]')
  scope=(
    '(--all-repos)--repo=[one repository]:repository (OWNER/NAME): '
    '(--repo)--all-repos[every repository involving you]'
  )
  view=(
    '--watched[only PRs you watch in PR Marmot]'
    '--no-color[plain text]'
    '--host=[GitHub host (github.com or an Enterprise Server host)]:host: '
    '--auth=[how to get a token]:mode:(auto gh device token)'
  )

  _arguments -C \
    $help \
    '(- *)'{-V,--version}'[show the version]' \
    '1:command:->command' \
    '*::argument:->argument' && ret=0

  case $state in
    command)
      local -a commands
      commands=(
        'mine:PRs you authored (My PRs), or every PR involving you with --all-repos'
        'review:PRs waiting for your review (Review queue)'
        'all:every open PR in one repository (All open); needs --repo'
        'watch:poll and print what changes, one event per line'
        'auth:sign in to GitHub without the gh CLI'
        'skill:print or install the coding-agent skill'
        'completions:print a shell completion script'
        'help:show help'
      )
      _describe -t commands command commands && ret=0
      ;;
    argument)
      curcontext=${curcontext%:*:*}:prmarmot-cli-$line[1]:
      case $line[1] in
        mine|authored|review|reviews|all|all-open)
          local -a board
          board=(
            '(-f --format --json)'{-f+,--format=}'[output format]:format:(table markdown json)'
            '(-f --format --json)--json[same as --format json]'
            '--changed[only PRs changed since you last looked in PR Marmot]'
            '--stale[only PRs that have waited too long for a reviewer]'
            '--filter=[only PRs matching a search query: words, label:, author:, repo:, is:stale]:query: '
            '--snoozed[show snoozed PRs instead of collapsing them]'
            '--pages=[result pages to load per queue]:pages:(1 2 3 4 5)'
          )
          case $line[1] in
            mine|authored)
              board+=('--authored[with --all-repos: only PRs you authored]')
              ;;
            review|reviews)
              board+=('--sort=[order inside the pickup sections, longest wait or smallest change first]:order:(wait smallest)')
              ;;
            *)
              # All open covers one repository.
              scope=('--repo=[one repository]:repository (OWNER/NAME): ')
              ;;
          esac
          _arguments -s $help $scope $view $board && ret=0
          ;;
        watch)
          watch=(
            '(-f --format --json)'{-f+,--format=}'[output format]:format:(text json)'
            '(-f --format --json)--json[JSON lines, same as --format json]'
            '--interval=[poll interval in seconds, at least 30]:seconds: '
            '(--until)--events=[exit after N change events]:count: '
            '--snoozed[include events for snoozed PRs]'
            '1::view:(mine review)'
          )
          # Following one PR, and --authored, belong to My PRs.
          [[ $words[2] == review* ]] || watch+=(
            '(--repo --all-repos --watched --authored)--pr=[one pull request, in any repository]:pull request (OWNER/NAME#N or URL): '
            '(--events)*--until=[stop once the PR reaches a condition]:condition:_sequence compadd - ci-pass approved mergeable merged'
            '--timeout=[give up after a duration]:duration:(90s 5m 30m 1h 2h)'
            '(--pr)--authored[with --all-repos: only PRs you authored]'
          )
          _arguments -s $help $scope $view $watch && ret=0
          ;;
        auth)
          _arguments -s $help \
            '1::action:(login status logout)' \
            '--with-token[read a personal access token from standard input]' \
            '--host=[GitHub host]:host: ' \
            '--client-id=[OAuth client ID for that host]:client id: ' && ret=0
          ;;
        skill)
          _arguments -s $help \
            '1::action:(install)' \
            '(--dir)--agent=[which agents read the skill]:agent:(claude agents all)' \
            '(--agent)--dir=[any other skills directory]:directory:_files -/' \
            '--force[replace a copy that differs or is a symlink]' && ret=0
          ;;
        completions)
          _arguments $help '1:shell:(bash zsh fish)' && ret=0
          ;;
      esac
      ;;
  esac
  return ret
}

if [[ $funcstack[1] == _prmarmot-cli ]]; then
  _prmarmot-cli "$@"
else
  compdef _prmarmot-cli prmarmot-cli
fi
