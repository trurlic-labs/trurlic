'use strict';

const path = require('path');

function resolveShelf(name) {
  return path.join('shelves', name);
}

function internal() {}

const helpers = {
  trim(text) {
    return text.trim();
  },
};

class Store {
  #count = 0;
  constructor(root) {
    this.root = root;
  }
  get count() {
    return this.#count;
  }
}

module.exports = { resolveShelf, Store, shared: helpers };
exports.extra = function () {};
