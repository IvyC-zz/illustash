// t() exactly as a Chinese page has it (web/static/i18n.js), for tests that run slices of app.js in a bare context.
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const context = {self: {ILLUSTASH_LANGUAGE: 'zh-CN'}, navigator: {}, document: {documentElement: {}}};
vm.createContext(context);
vm.runInContext(readFileSync(require('node:path').join(__dirname, '../web/static/i18n.js'), 'utf8'), context);
module.exports = vm.runInContext('t', context);
