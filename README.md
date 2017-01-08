[![Build Status](https://travis-ci.org/toshipp/showfs.svg?branch=master)](https://travis-ci.org/toshipp/showfs)

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
