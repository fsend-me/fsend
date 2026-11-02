# fsend shell helper (bash / zsh)
#
# Load it once, or add this line to ~/.bashrc / ~/.zshrc:
#   eval "$(curl -fsSL __FSEND_ORIGIN__/install.sh)"
# or save it: curl -fsSL __FSEND_ORIGIN__/install.sh > ~/.fsend.sh && echo "source ~/.fsend.sh" >> ~/.zshrc
#
# Usage:
#   fsend [options] <file>
#   cat file | fsend [options] <name>
#
# Options:
#   -t, --token <token>     upload token (default: $FSEND_TOKEN)
#   -d, --days <days>       expire after N days (decimals allowed, e.g. 0.5)
#   -n, --downloads <n>     delete after N downloads
#   -u, --url <url>         server (default: $FSEND_URL or __FSEND_ORIGIN__)
#   -h, --help

fsend() {
  local token="${FSEND_TOKEN:-}" url="${FSEND_URL:-__FSEND_ORIGIN__}"
  local days="" downloads="" file="" src="" name=""
  while [ $# -gt 0 ]; do
    case "$1" in
      -t|--token)     token="$2"; shift 2 ;;
      --token=*)      token="${1#*=}"; shift ;;
      -d|--days)      days="$2"; shift 2 ;;
      --days=*)       days="${1#*=}"; shift ;;
      -n|--downloads) downloads="$2"; shift 2 ;;
      --downloads=*)  downloads="${1#*=}"; shift ;;
      -u|--url)       url="$2"; shift 2 ;;
      --url=*)        url="${1#*=}"; shift ;;
      -h|--help)      cat <<'EOF'
Usage:
  fsend [options] <file>
  cat file | fsend [options] <name>

Options:
  -t, --token <token>     upload token (default: $FSEND_TOKEN)
  -d, --days <days>       expire after N days (decimals allowed, e.g. 0.5)
  -n, --downloads <n>     delete after N downloads
  -u, --url <url>         server (default: $FSEND_URL)
  -h, --help
EOF
                      return 0 ;;
      -*)             echo "fsend: unknown option: $1" >&2; return 1 ;;
      *)              file="$1"; shift ;;
    esac
  done

  if [ -z "$file" ]; then
    echo "usage: fsend [--token T] [--days D] [--downloads N] <file>" >&2; return 1
  fi
  if [ -f "$file" ]; then
    src="$file"; name="$(basename "$file")"
  elif [ ! -t 0 ]; then
    src="-"; name="$file"
  else
    echo "fsend: no such file: $file" >&2; return 1
  fi

  local -a args
  args=(--fail-with-body --progress-bar --upload-file "$src")
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  [ -n "$days" ] && args+=(-H "Max-Days: $days")
  [ -n "$downloads" ] && args+=(-H "Max-Downloads: $downloads")

  # percent-encode the filename
  local enc
  enc="$(printf %s "$name" | od -An -tx1 | tr -d ' \n' | sed 's/../%&/g')"
  curl "${args[@]}" "${url%/}/$enc"
}
