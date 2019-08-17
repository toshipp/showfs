[![CircleCI](https://circleci.com/gh/toshipp/showfs.svg?style=svg)](https://circleci.com/gh/toshipp/showfs)

showfs - mount an archive as a directory.
-----------------------------------------

* build

    ```
    cargo build
    ```

* test

    ```
    ./tool/make_assets
    cargo test
    ```

* usage

    ```
    showfs $ARCHIVE $DIR
    showfs $DIR_CONTAINING_ARCHIVE $DIR
    ```
