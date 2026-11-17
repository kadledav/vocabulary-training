// @ts-check
/**
 * Synthetic packs for the e2e tests: invented headwords and sentences
 * written for the tests, `test-` source and pack ids only (design §14.5).
 */

/**
 * @param {string} en marked English sentence (`[[…]]` around the target)
 * @param {string} cs marked Czech translation
 * @param {string} [location]
 */
function sentence(en, cs, location) {
  return { en, cs, origin: 'example', location: location ?? null };
}

/**
 * The book pack. New words are introduced by usefulness, so a fresh block
 * shows them in this order: glimmerous (sentence format, 3 sentences),
 * brontle (word-only, 1 sentence), quabble (sentence), snorkish (word-only,
 * 2 sentences).
 */
export const BOOK_PACK = {
  format: 'vocab-pack',
  format_version: 1,
  pack_id: 'test-e2e-novel--2026-11-01--1',
  exported_at: '2026-11-01',
  source: { id: 'test-e2e-novel', type: 'book', title: 'An E2E Test Novel' },
  reading_position: 'Chapter 7',
  words: [
    {
      headword: 'glimmerous',
      meaning: 'Shining faintly and unsteadily.',
      czech: ['třpytivý', 'mihotavý'],
      usefulness: 9,
      pronunciation: 'GLIM-er-us',
      speak: 'glimmerous',
      icon: '✨',
      note: 'An invented test word: used for light that comes and goes.',
      hint: 'Think of a candle in a draught.',
      forms: [
        { form: 'glimmerous', common: true },
        { form: 'glimmerously', common: false },
      ],
      sentences: [
        sentence('The [[glimmerous]] lamp swung above the door.', 'Nad dveřmi se houpala [[mihotavá]] lampa.', 'Chapter 1'),
        sentence('Her eyes turned [[glimmerous]] in the dusk.', 'Její oči byly v šeru [[třpytivé]].', 'Chapter 2'),
        sentence('A [[glimmerous]] path led to the lake.', 'K jezeru vedla [[třpytivá]] cesta.', 'Chapter 3'),
      ],
    },
    {
      headword: 'brontle',
      meaning: 'To grumble quietly while working.',
      czech: ['bručet si'],
      usefulness: 8,
      pronunciation: 'BRON-tul',
      note: 'An invented test word.',
      sentences: [sentence('The old cook would [[brontle]] over the pots.', 'Starý kuchař si nad hrnci [[bručel]].', 'Chapter 4')],
    },
    {
      headword: 'quabble',
      meaning: 'A small, pointless argument.',
      czech: ['hádka', 'šarvátka'],
      usefulness: 7,
      sentences: [
        sentence('They had a [[quabble]] about the map.', 'Měli kvůli mapě [[šarvátku]].'),
        sentence('Another [[quabble]] started at breakfast.', 'U snídaně začala další [[hádka]].'),
        sentence('The [[quabble]] ended as fast as it began.', '[[Hádka]] skončila stejně rychle, jak začala.'),
      ],
    },
    {
      headword: 'snorkish',
      meaning: 'Sleepy and slightly grumpy.',
      czech: ['rozespalý'],
      usefulness: 6,
      sentences: [
        sentence('He was [[snorkish]] until noon.', 'Až do poledne byl [[rozespalý]].'),
        sentence('A [[snorkish]] cat lay on the stairs.', 'Na schodech ležela [[rozespalá]] kočka.'),
      ],
    },
  ],
};

/** A second source (scope chooser). */
export const ARTICLE_PACK = {
  format: 'vocab-pack',
  format_version: 1,
  pack_id: 'test-e2e-article--2026-11-01--1',
  exported_at: '2026-11-01',
  source: { id: 'test-e2e-article', type: 'article', title: 'An E2E Test Article' },
  words: [
    {
      headword: 'flumpet',
      meaning: 'A dull, heavy sound.',
      czech: ['žuchnutí'],
      usefulness: 5,
      sentences: [sentence('The sack fell with a [[flumpet]].', 'Pytel spadl s [[žuchnutím]].')],
    },
  ],
};
