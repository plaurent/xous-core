# Book Client

A precursor application for reading plaintext books hosted on a remote server.


## Setup

Use shellchat to set a key in pddb for `book:url` to point to your book server:

```
pddb write book:url myserver.com
```

The book client will store an additional value in the pddb, `book:idx`, which keeps track of which book is currently being read.  The client-server setup currently supports reading up to 10 different books at a time (indexed 0 through 9).

## Controls

- Previous page: "Backspace"
- More lines per page: "+" or "h"
- Fewer lines per page: "-" or "g"
- Toggle brightness intensity (and move to next page): "!"
- Switch books: any key in the range "0".."9"
- Next page: press any other key (I like to use space, or a direction on the dpad)


# Book Server

Currently the books are served page-by-page from a remote server.

## Endpoints

The server API supports the following endpoints:

- `/book/next[?bookindex=<n>]` -- returns the next page ("limit" lines) of text
- `/book/prev[?bookindex=<n>]` -- returns the previous page ("limit" lines) of text
- `/book/more[?bookindex=<n>]` -- increases the limit / number of returned lines per request by 1
- `/book/less[?bookindex=<n>]` -- decreases the limit / number of returned lines per request by 1
- `/book/list[?bookindex=<n>]` -- shows a list of the books, with an asterisk in front of the passed-in bookindex

All endpoints have an optional parameter `?bookindex=<n>` which specifies which book number is being currently read. If no bookindex is passed, the server conveniently assumes you're using book 0.

## Internal data format


```
cat read.json | json_pp
[
   {
      "file" : "./xousbook.txt",
      "limit" : 7,
      "line" : 657
   },
   {
      "file" : "./donquixote.txt",
      "limit" : 7,
      "line" : 2768
   },
   {
      "file" : "./arabiannights.txt",
      "limit" : 7,
      "line" : 22
   }
]
```

The server internally keeps track of the last read line per book, and the "pagination" size (how many lines to return per page) using a parameter "limit".


# Wishlist

- Future versions of the `book` client may download and cache from the remote server.
- Future versions of the `book` server may retrieve, cache and serve Project Gutenberg books upon request from the client.
