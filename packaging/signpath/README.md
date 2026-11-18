# Code signing with SignPath

ChimeraIRC's releases are meant to be signed through the [SignPath Foundation](https://signpath.org)'s free program for
open-source projects. The build runs on GitHub (`.github/workflows/release.yml`), hands the program and the installer to
SignPath, and publishes what comes back. Until the project is set up there, the same workflow makes unsigned files.

## One-time setup

1. **Turn on two-factor sign-in** for the GitHub account (and later for SignPath). The program requires it for everyone
   on the team.
2. **Apply** at <https://signpath.org/apply> for the project "ChimeraIRC". It qualifies as open source (GPL-3.0-or-later),
   has a public repository and has releases. The answers are in `docs/CODE_SIGNING_POLICY.md`; link to it as the
   project's "Code signing policy".
3. After approval, in SignPath:
   * create the **project** (slug `chimerairc`) and link the trusted build system **GitHub.com** to it;
   * add the two **artifact configurations** from this folder, with the slugs `exe` (`exe.xml`) and `msi` (`msi.xml`);
   * create the **signing policy** `release-signing` (manual approval by the approver);
   * install the [SignPath GitHub app](https://github.com/apps/signpath) for the repository;
   * create an **API token** for a user who may submit requests.
4. In the GitHub repository (Settings → Secrets and variables → Actions):
   * secret `SIGNPATH_API_TOKEN`: the token;
   * variable `SIGNPATH_ORGANIZATION_ID`: the organization id shown in SignPath;
   * optional variables `SIGNPATH_PROJECT_SLUG` and `SIGNPATH_SIGNING_POLICY_SLUG` if you chose other slugs.
5. Add the line from the policy to the README and the download page, and change the note about unsigned downloads.

SignPath's rules also want the product name and version of the signed files to be fixed in the artifact configuration
(`ChimeraIRC`, the same version on every build); the SignPath screen for the configuration offers those settings.

## Making a release

1. Raise the version in `Cargo.toml` (and the roadmap), commit and push.
2. `git tag v0.1.6` then `git push origin v0.1.6`.
3. The workflow builds and tests, asks SignPath to sign the program, builds the installer and ZIP from the signed
   program, asks SignPath to sign the installer, and creates the release (a pre-release, with generated notes you can
   edit). Each signing request waits until the approver approves it in SignPath, so two approvals per release.

`workflow_dispatch` runs the same steps by hand without publishing, handy for trying changes.

## Building by hand

`packaging\build-installer.ps1` still works on its own. With `-ExePath` it packages an exe that is already built (a signed
one, for example).
