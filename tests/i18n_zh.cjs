// t() exactly as a Chinese page has it (web/static/i18n.js), for tests that run slices of app.js in a bare context.
// Its page property also holds tb() and said() for the backend's text: Object.assign(context, zh.page).
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const context = {self: {ILLUSTASH_LANGUAGE: 'zh-CN'}, navigator: {}, document: {documentElement: {}}};
vm.createContext(context);
vm.runInContext(readFileSync(require('node:path').join(__dirname, '../web/static/i18n.js'), 'utf8'), context);
const [t, tb, said] = ['t', 'tb', 'said'].map(name => vm.runInContext(name, context));
module.exports = Object.assign(t, {page: {t, tb, said}});
