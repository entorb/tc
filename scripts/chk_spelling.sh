#!/bin/sh
set -e
cd "$(dirname "$0")/.."

status=0
pnpm dlx cspell-cli@10.0.1 --unique --words-only . >cspell-words-missing.txt 2>/dev/null || status=$?

if [ $status -ne 0 ]; then
  # V1
  # echo "Found unknown spellings, see cspell-words-missing.txt. Fix or transfer to cspell-words.txt"
  # V2
  # make entries in cspell-words-missing.txt lowercase
  sed -i 's/.*/\L&/' cspell-words-missing.txt
  # append to cspell-words.txt
  cat cspell-words-missing.txt >>cspell-words.txt
  # run prek hook to sort file and remove duplicates
  prek run --all-files --quiet
  echo OK

else
  rm cspell-words-missing.txt
  echo OK
fi

exit $status
