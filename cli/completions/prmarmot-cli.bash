# bash completion for prmarmot-cli
#
# Install with bash-completion:
#   prmarmot-cli completions bash > ~/.local/share/bash-completion/completions/prmarmot-cli
# or load it from ~/.bashrc:
#   eval "$(prmarmot-cli completions bash)"

_prmarmot_cli() {
    local cur=${COMP_WORDS[COMP_CWORD]}
    local command="" action="" flagged="" word words i
    local value_flags=" --repo -f --format --pages --sort --interval --events --pr --until --timeout --agent --dir --host --auth --client-id "

    # The command and its first word, skipping flag values
    # (`--flag value`, or `--flag = value` once bash splits at "=").
    for ((i = 1; i < COMP_CWORD; i++)); do
        word=${COMP_WORDS[i]}
        if [[ $value_flags == *" $word "* ]]; then
            [[ -n $command ]] && flagged=1
            ((i++))
            [[ ${COMP_WORDS[i]} == "=" ]] && ((i++))
            continue
        fi
        case $word in
            -*) [[ -n $command ]] && flagged=1 ;;
            *)
                if [[ -z $command ]]; then
                    command=$word
                elif [[ -z $action ]]; then
                    action=$word
                fi
                ;;
        esac
    done

    local flag=${COMP_WORDS[COMP_CWORD - 1]}
    if [[ $cur == "=" ]]; then
        cur=""
    elif [[ $flag == "=" && $COMP_CWORD -ge 2 ]]; then
        flag=${COMP_WORDS[COMP_CWORD - 2]}
    fi
    case $flag in
        -f | --format)
            if [[ $command == watch ]]; then
                words="text json"
            else
                words="table markdown json"
            fi
            COMPREPLY=($(compgen -W "$words" -- "$cur"))
            return
            ;;
        --until)
            # A comma-separated list: complete its last item.
            local done="" last=$cur
            if [[ $cur == *,* ]]; then
                done=${cur%,*},
                last=${cur##*,}
            fi
            COMPREPLY=($(compgen -P "$done" -W "ci-pass approved mergeable merged" -- "$last"))
            return
            ;;
        --pages)
            COMPREPLY=($(compgen -W "1 2 3 4 5" -- "$cur"))
            return
            ;;
        --sort)
            COMPREPLY=($(compgen -W "wait smallest" -- "$cur"))
            return
            ;;
        --timeout)
            COMPREPLY=($(compgen -W "90s 5m 30m 1h 2h" -- "$cur"))
            return
            ;;
        --agent)
            COMPREPLY=($(compgen -W "claude agents all" -- "$cur"))
            return
            ;;
        --auth)
            COMPREPLY=($(compgen -W "auto gh device token" -- "$cur"))
            return
            ;;
        --dir)
            compopt -o filenames 2>/dev/null
            local IFS=$'\n'
            COMPREPLY=($(compgen -d -- "$cur"))
            return
            ;;
        --filter)
            # A free-text query; offer the qualifier words as a starting point.
            COMPREPLY=($(compgen -W "label: author: repo: is:stale" -- "$cur"))
            compopt -o nospace 2>/dev/null
            return
            ;;
        --repo | --pr | --interval | --events | --host | --client-id)
            COMPREPLY=()
            return
            ;;
    esac

    local view="--repo --all-repos --format --json --watched --snoozed --no-color --help --host --auth"
    case $command in
        "")
            if [[ $cur == -* ]]; then
                words="--help --version"
            else
                words="mine review all watch auth skill completions help"
            fi
            ;;
        mine | authored)
            words="$view --authored --changed --stale --filter --pages"
            ;;
        review | reviews)
            words="$view --changed --stale --filter --sort --pages"
            ;;
        all | all-open)
            # One repository only, so no --all-repos.
            words="${view/ --all-repos/} --changed --stale --filter --pages"
            ;;
        watch)
            # A view word only right after `watch`.
            if [[ -z $action && -z $flagged && $cur != -* ]]; then
                words="mine review"
            elif [[ $action == review* ]]; then
                words="$view --interval --events"
            else
                words="$view --authored --interval --events --pr --until --timeout"
            fi
            ;;
        auth)
            if [[ -z $action ]]; then
                words="login status logout --help"
            elif [[ $action == login ]]; then
                words="--with-token --host --client-id --help"
            else
                words="--host --help"
            fi
            ;;
        skill)
            if [[ -z $action ]]; then
                words="install --help"
            else
                words="--agent --dir --force --help"
            fi
            ;;
        completions)
            [[ -z $action ]] && words="bash zsh fish"
            ;;
    esac
    COMPREPLY=($(compgen -W "$words" -- "$cur"))
}

complete -F _prmarmot_cli prmarmot-cli
