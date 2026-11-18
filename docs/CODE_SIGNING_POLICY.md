# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

> **Status:** the signing set-up is being arranged (see `packaging/signpath/README.md`). Until it is finished, releases
> are not signed and Windows SmartScreen may say "unknown publisher". This line is removed from here once the
> releases are signed.

## What is signed

The program `chimerairc.exe` and the installer `ChimeraIRC-<version>-x64.msi`, built from this repository by the GitHub
workflow `.github/workflows/release.yml` on GitHub-hosted machines. Nothing else is signed, and nothing that was not
built from the public source is ever submitted.

## Roles

| Role | Who |
|---|---|
| Authors (commit to the repository) | Howard E. DeWitt, [@theG33K](https://github.com/theG33K) |
| Reviewers (review changes from anyone else) | Howard E. DeWitt |
| Approvers (approve every signing request) | Howard E. DeWitt |

Everyone with a role signs in to GitHub and SignPath with two-factor authentication. Every release needs a manual approval
of its signing requests.

## Privacy

ChimeraIRC does not send any information to other networked systems unless the user asks for it: it connects to the IRC
servers the user chooses, and (only if the user turns on **Options > Check for Updates at Start**, or chooses **Help >
Check for Updates**) asks GitHub for the list of releases, sending nothing but the program name and version. Passwords are
never stored. See the README, "Privacy and safety".

## Reporting a problem

Open an issue at <https://github.com/theG33K/chimerairc/issues>.
