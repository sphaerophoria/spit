# spit (sphaerophoria's git)

# Motivation to exist
I'm comfortable on the git command line. After trying many git guis, I haven't
found one that makes my workflows easier. The git guis I've tried seem to be
tailored to people who don't want to (or don't know how to) use git's command
line. The git guis I've tried also seem to feel very slow. E.g. loading the
linux repository and scrolling to the bottom can be measured in minutes (it is
also noticeably extremely slow with git log itself).

I want a git gui that either makes my existing workflows easier, or makes them
better.

# Principles 
* spit is (or should be) fast
* spit is intended to work in harmony with the git command line, not replace it
* spit will not modify remote repositories for you
* spit will not protect you from mistakes on a local repo. git reflog is your
  friend
* spit should make reviewing code and browsing history easy
* spit should make it easier to do any command line operation that interacts
  with a commit/branch/tag/etc.

# Features
* Fast commit view (from cold start)
  * On my machine opening the linux repository and viewing the first commit
    takes ~7s. `git log` takes ~20s to get to the bottom. Many git guis I
    couldn't even get there
  * Using spit should be fast enough to replace using `git log`. Typing `spit
    .&` is effectively instant for most repositories I work in
* Reasonable log view
* Responsive branch filtering/sorting
* Integrated command line 
* Context menu actions for many common operations (append to command, checkout,
  delete, cherry-pick, merge, etc.)
