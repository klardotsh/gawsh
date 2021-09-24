# gawsh: a static site generator to peruse git repositories

> well gawsh darn, that's a lot of HTML...

`gawsh` generates a static HTML portrait of a Git repository, outputting
standards-compliant HTML fragments (or, optionally, simple-but-full HTML5
pages) to allow basic perusing of Git repositories on the web. It's designed to 

### Features

- output files should be legible, even if not perfect, in anything capable of
  rendering basic, standards-compliant HTML5 fragments.  In general, this means
  `gawsh` sites should work to some basic degree (or better) in browsers like
  [Lynx](https://invisible-island.net/lynx/) or
  [Netsurf](https://www.netsurf-browser.org/)

### Non-Features

- anything related to social networking, popularity, or for that matter,
  anything that requires a deeper understanding of a "user" than the name
  and/or email address associated with a commit in the commit log

- anything related to analytics, telemetry, or other forms of user-agent
  tracking

- anything related to standing up Git repository hosting in general: Bring Your
  Own Repo

- anything related to deployment, CI/CD, etc., however examples are provided in
  the documentation on how to set up such contraptions yourself

## Getting Started

Start off by acquiring a copy of `gawsh`, either from your system package
manager, via the official Docker image (`docker.io/klardotsh/gawsh`) or a local
build thereof, or by building from Rust source with `cargo`.

> If you choose to build `gawsh` from source, you'll need a Rust 1.55 compiler
> or newer (older Rusts may work, but have not been tested) on a platform that
> `libgit2` can be built for (this should include modern Windows, Linux, MacOS,
> and most BSDs, as `gawsh` doesn't depend on any of the SSH or SSL optional
> functionality).

`gawsh` is entirely configured with CLI flags, and in general is intentionally
inflexible in its output, preferring to generate simple HTML that's easy to
mangle with external tooling (or to style with CSS) over allowing infinite
templating possibilities (and absorbing all the complexity that entails). Thus,
here's the output of `gawsh --help`.

```
Usage: gawsh [-v] [-j <jobs>] [-C <repository>] [-o <output>] [--templating-behavior <templating-behavior>] [-P]

gawsh generates a static HTML portrait of a Git repository

Options:
  -v, --verbose     be chatty
  -j, --jobs        maximum number of parallel jobs, defaults to number of CPU
                    cores. bigger numbers are not always better, depending on
                    the speed of your drives, amount of RAM, etc.
  -C, --repository  repository to operate on, defaults to current directory
  -o, --output      output directory for rendered files, will be created if it
                    doesn't exist. defaults to ./.gawsh-output
  --templating-behavior
                    templating behavior for embedding rendered Objects into tree
                    files
  -P, --use-class-prefix
                    prefix highlighting HTML classes with gawsh- to avoid CSS
                    collisions
  --help            display usage information
```


## Self-hostable alternatives

### Static Generation

- [stagit](https://codemadness.org/stagit.html) is also excellent and worth
  your consideration. It makes a few different tradeoffs regarding features and
  performance, but was the primary influence for `gawsh`

### Dynamic (CGI/Web Apps)

- [cgit](https://git.zx2c4.com/cgit/)
- [Gitea](https://gitea.com/)
- [Gogs](https://gogs.io/)
- [Sourcehut](https://sourcehut.org/)
- [GitLab](https://gitlab.com/)

## Copying, Contributing, and Legal

`gawsh`'s implementation, specification, documentation, artwork, and other
assets are all [Copyfree](http://copyfree.org/), released under the [Creative
Commons Zero 1.0
dedication](https://creativecommons.org/publicdomain/zero/1.0/). This means
you're free to use it for any purpose, in any context, and without letting me
know.

Contributions will be considered, but are not guaranteed to be merged for any
reason or no reason at all. By submitting a contribution to `gawsh`, you assert
the following (this is the [Unlicense waiver](https://unlicense.org/WAIVER)):

> I dedicate any and all copyright interest in this software to the
> public domain. I make this dedication for the benefit of the public at
> large and to the detriment of my heirs and successors. I intend this
> dedication to be an overt act of relinquishment in perpetuity of all
> present and future rights to this software under copyright law.
>
> To the best of my knowledge and belief, my contributions are either
> originally authored by me or are derived from prior works which I have
> verified are also in the public domain and are not subject to claims
> of copyright by other parties.
>
> To the best of my knowledge and belief, no individual, business,
> organization, government, or other entity has any copyright interest
> in my contributions, and I affirm that I will not make contributions
> that are otherwise encumbered.
