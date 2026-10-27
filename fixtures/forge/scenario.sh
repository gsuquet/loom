#!/bin/sh
# Builds the git side of the forge fixtures: a bare repository that holds the
# branches and merge request refs the JSON files describe. Commits use fixed
# names and dates, so the SHAs are the same on every machine and match the
# JSON files.
#
# Usage: scenario.sh <directory>   (the directory must not exist)
set -eu

if [ "$#" -ne 1 ] || [ -e "$1" ]; then
  echo "usage: scenario.sh <new directory>" >&2
  exit 2
fi

out=$1
work=$out/work
bare=$out/shop.git

git_() {
  git -C "$work" -c commit.gpgsign=false -c core.autocrlf=false -c init.defaultBranch=main "$@"
}

commit() {
  # $1: date, $2: author name, $3: message
  GIT_AUTHOR_NAME=$2 GIT_AUTHOR_EMAIL="$2@example.com" GIT_AUTHOR_DATE="$1" \
  GIT_COMMITTER_NAME=$2 GIT_COMMITTER_EMAIL="$2@example.com" GIT_COMMITTER_DATE="$1" \
    git_ commit -q -m "$3"
}

mkdir -p "$work/src/auth" "$work/src/api"
git -c init.defaultBranch=main init -q "$work"

# M1: the base of version 1.
cat > "$work/README.md" <<'X'
# shop
X
cat > "$work/src/auth/mod.rs" <<'X'
pub mod jwt;

pub fn login_required(path: &str) -> bool {
    path.starts_with("/account")
}
X
cat > "$work/src/auth/jwt.rs" <<'X'
use crate::clock::now;

pub struct Claims {
    pub sub: String,
    pub exp: u64,
}

pub fn verify(token: &str) -> Result<Claims, Error> {
    let claims = decode(token)?;
    if claims.exp < now() {
        return Err(Error::Expired);
    }
    Ok(claims)
}
X
cat > "$work/src/api/login.rs" <<'X'
use crate::auth::jwt;

pub fn login(token: &str) -> Response {
    match jwt::verify(token) {
        Ok(claims) => Response::ok(claims.sub),
        Err(_) => Response::unauthorized(),
    }
}
X
printf '# lock v1\n' > "$work/Cargo.lock"
git_ add -A
commit "2026-10-20T09:00:00Z" alice "Initial commit"

# Version 1 of the merge request, on M1.
git_ checkout -q -b auth-refactor
cat > "$work/src/auth/jwt.rs" <<'X'
use crate::clock::now;

pub struct Claims {
    pub sub: String,
    pub exp: u64,
}

pub fn verify(token: &str) -> Result<Claims, Error> {
    let claims = decode(token)?;
    if claims.exp <= now() {
        return Err(Error::Expired);
    }
    if claims.sub.is_empty() {
        return Err(Error::NoSubject);
    }
    Ok(claims)
}
X
cat > "$work/src/api/login.rs" <<'X'
use crate::auth::jwt;

pub fn login(token: &str) -> Response {
    match jwt::verify(token) {
        Ok(claims) => Response::ok(claims.sub),
        Err(jwt::Error::Expired) => Response::expired(),
        Err(_) => Response::unauthorized(),
    }
}
X
printf '# lock v2\n' > "$work/Cargo.lock"
git_ add -A
commit "2026-10-21T10:00:00Z" alice "Refactor token checks"
v1=$(git_ rev-parse HEAD)

# M2: main moves after version 1.
git_ checkout -q main
printf '# shop\n\nAn example shop.\n' > "$work/README.md"
git_ add -A
commit "2026-10-22T08:00:00Z" bob "Describe the shop"

# Version 2: version 1 rebased on M2, plus a leeway, pushed with force.
git_ checkout -q auth-refactor
GIT_COMMITTER_NAME=alice GIT_COMMITTER_EMAIL=alice@example.com GIT_COMMITTER_DATE="2026-10-23T11:00:00Z" \
  git_ rebase -q main
sed 's/if claims.exp <= now() {/if claims.exp + LEEWAY <= now() {/' "$work/src/auth/jwt.rs" > "$work/jwt.tmp"
mv "$work/jwt.tmp" "$work/src/auth/jwt.rs"
git_ add -A
commit "2026-10-23T11:05:00Z" alice "Allow a clock leeway"
v2=$(git_ rev-parse HEAD)

git clone -q --bare "$work" "$bare"
# The forges keep the head of each merge request under their own refs.
git -C "$bare" update-ref refs/pull/412/head "$v2"
git -C "$bare" update-ref refs/merge-requests/88/head "$v2"
git -C "$bare" update-ref refs/loom-fixture/v1 "$v1"
rm -rf "$work"

echo "$bare"
