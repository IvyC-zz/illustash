// Names the page gives to the backend's kinds (#199): why a picture was excluded, how two pictures relate. No DOM.
import {t} from './i18n.js';
export const REASONS = {photo: t('真人/照片'), screenshot: t('截图'), text: t('文字图'), manual: t('手动排除')};
// How two look-alike pictures relate; which of these fold into one is a library setting.
export const RELATION_NAMES = {identical: t('完全相同'), duplicate: t('重复'), subtle: t('细微差分'), variant: t('差分'), crop: t('局部重复'), edit: t('局部编辑')};
export const FOLD_OPTIONS = [['identical', RELATION_NAMES.identical, t('像素一样的不同文件')], ['duplicate', RELATION_NAMES.duplicate, t('同一张图缩放或重新压缩过')], ['subtle', RELATION_NAMES.subtle, t('只差表情、签名、小物件（< 2%）')],
  ['edit', RELATION_NAMES.edit, t('加字水印、调色、色块或马赛克')], ['crop', RELATION_NAMES.crop, t('一张是另一张的裁剪')], ['variant', RELATION_NAMES.variant, t('同一张画改了局部或重新上色')]];
export const RELATION_KINDS = [['', t('全部')], ...['duplicate', 'subtle', 'variant', 'crop', 'edit'].map(k => [k, RELATION_NAMES[k]])];
// Display names come from the server (中文 → 日文 → readable English); the original tag only shows on hover.
export const nameTip = c => !c ? '' : c.tag && c.tag !== c.name ? c.tag + (c.name_source === 'model' ? ' · ' + t('机器翻译') : c.name_source === 'wiki' ? ' · ' + t('来自 Danbooru 别名') : '') : c.name;
