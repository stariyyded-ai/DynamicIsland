// Little cartoon pets that sit next to the island. Drawn as vector SVG so they stay crisp at any size.
(function(){
  const COLORS = {
    dragon: [
      {name:'Уголёк',  b:'#2c3140', s:'#1a1d27', l:'#454c60', e:'#8be04e', w:'#1f2330', wm:'#3a4155', h:'#5d6680'},
      {name:'Изумруд', b:'#33a865', s:'#1d6e40', l:'#a6e6bd', e:'#ffd23f', w:'#23824c', wm:'#58c487', h:'#e9f7c8'},
      {name:'Рубин',   b:'#d2434f', s:'#8e2330', l:'#f7b0b6', e:'#ffd23f', w:'#a82e3b', wm:'#ec7480', h:'#ffe3c2'},
      {name:'Сапфир',  b:'#3f78e0', s:'#21479a', l:'#b4cdff', e:'#ffe066', w:'#2c5bb8', wm:'#6f9cf0', h:'#e2ecff'},
      {name:'Золото',  b:'#eab034', s:'#a0701a', l:'#ffe7a6', e:'#41c3ff', w:'#c48a1f', wm:'#f5cc6a', h:'#fff3cc'}
    ],
    cat: [
      {name:'Серый',    b:'#9097a6', s:'#5f6573', l:'#e6e8ee', e:'#9be15d', n:'#f08aa3', i:'#f3b3c3'},
      {name:'Рыжий',    b:'#f2a24c', s:'#c2702a', l:'#fff0dc', e:'#5ec46a', n:'#e9768f', i:'#f8c2b4'},
      {name:'Чёрный',   b:'#2d3038', s:'#17181d', l:'#4a4e5a', e:'#ffd23f', n:'#e48aa0', i:'#c97a8f'},
      {name:'Белый',    b:'#f6f6f8', s:'#cdd0d8', l:'#ffffff', e:'#4cb7ff', n:'#f08aa3', i:'#f8c6d2'},
      {name:'Сиамский', b:'#efe3cf', s:'#6b5240', l:'#fbf5ea', e:'#4c9cff', n:'#7a5c46', i:'#9b7a62'}
    ],
    fox: [
      {name:'Рыжий',    b:'#f2802f', s:'#bf5617', l:'#fff6ea', e:'#3a1d0b', n:'#2a1810', t:'#2a1810'},
      {name:'Снежный',  b:'#eef2f7', s:'#b9c2cf', l:'#ffffff', e:'#3c4452', n:'#3c4452', t:'#8d97a6'},
      {name:'Серебро',  b:'#808a9b', s:'#4f5766', l:'#e7ebf1', e:'#1c1f25', n:'#1c1f25', t:'#1c1f25'},
      {name:'Песчаный', b:'#e8c47e', s:'#b38b44', l:'#fff7e2', e:'#4a3412', n:'#4a3412', t:'#4a3412'},
      {name:'Огненный', b:'#e44a3c', s:'#a3271e', l:'#ffeadb', e:'#3a0c08', n:'#24100c', t:'#24100c'}
    ]
  };
  const NAMES = {dragon:'Дракончик', cat:'Котик', fox:'Лисёнок'};
  const W = 50, H = 46;
  const K = '#14161c';
  let uid = 0;

  // eyes: open (iris, pupil, shine), closed (a soft arc) or happy (^ ^)
  function eyes(p, o, x1, x2, y, rx, ry, slit){
    const one = x => {
      if (o.closed) return '<path d="M' + (x - rx) + ' ' + y + ' q' + rx + ' ' + (ry*.7) + ' ' + (2*rx) + ' 0" fill="none" stroke="' + K + '" stroke-width="1.8" stroke-linecap="round"/>';
      if (o.happy) return '<path d="M' + (x - rx) + ' ' + (y + 1) + ' q' + rx + ' ' + (-ry*1.1) + ' ' + (2*rx) + ' 0" fill="none" stroke="' + K + '" stroke-width="2" stroke-linecap="round"/>';
      return '<ellipse cx="' + x + '" cy="' + y + '" rx="' + rx + '" ry="' + ry + '" fill="' + p.e + '" stroke="' + K + '" stroke-width="1"/>' +
        (slit ? '<ellipse cx="' + x + '" cy="' + y + '" rx="' + (rx*.3) + '" ry="' + (ry*.78) + '" fill="#0d0e12"/>'
              : '<circle cx="' + x + '" cy="' + (y + .4) + '" r="' + (Math.min(rx, ry)*.62) + '" fill="#0d0e12"/>') +
        '<circle cx="' + (x - rx*.35) + '" cy="' + (y - ry*.4) + '" r="' + (rx*.32) + '" fill="#fff"/>' +
        '<circle cx="' + (x + rx*.3) + '" cy="' + (y + ry*.35) + '" r="' + (rx*.15) + '" fill="#fff" opacity=".8"/>';
    };
    return one(x1) + one(x2);
  }
  const blush = (x1, x2, y) => '<ellipse cx="' + x1 + '" cy="' + y + '" rx="3" ry="1.8" fill="#ff7a9a" opacity=".35"/><ellipse cx="' + x2 + '" cy="' + y + '" rx="3" ry="1.8" fill="#ff7a9a" opacity=".35"/>';
  const mouth = (o, y) => o.happy
    ? '<path d="M27.5 ' + y + ' q2.5 3.6 5 0 z" fill="#5a1e2a" stroke="' + K + '" stroke-width=".9" stroke-linejoin="round"/>'
    : '<path d="M27.6 ' + y + ' q1.2 1.5 2.4 0 q1.2 1.5 2.4 0" fill="none" stroke="' + K + '" stroke-width="1.1" stroke-linecap="round"/>';
  const ln = (extra) => ' stroke="' + K + '" stroke-width="' + (extra || 1) + '" stroke-linejoin="round"';

  function dragon(p, o, g){
    const wing = side => {
      const mirror = side > 0 ? ' transform="translate(60 0) scale(-1 1)"' : '';
      // tucked small behind the head at rest; spread out for a happy flap
      const rot = o.flap ? ' transform="translate(20 36) rotate(-12) scale(.95) translate(-20 -36)"' : ' transform="translate(21 36) rotate(32) scale(.62) translate(-20 -36)"';
      return '<g' + mirror + '><g' + rot + '><path d="M20 34 C12 30 5 24 3 15 C7 18 9 18 11 17 C10 21 12 23 14 23 C14 27 16 29 19 29 Z" fill="' + p.w + '"' + ln() + '/>' +
        '<path d="M19 32 C13 28 8 22 6 18 M19 31 C15 27 13 24 12 20" fill="none" stroke="' + p.wm + '" stroke-width=".9" stroke-linecap="round" opacity=".8"/></g></g>';
    };
    return '<path d="M40 47 C50 48 55 43 53 36 C52 33 49 34 50 37 C51 41 47 43 40 42 Z" fill="' + p.b + '"' + ln() + '/>' +
      '<path d="M52 35 l3 -3 l-0.5 4 z" fill="' + p.h + '"' + ln(.7) + '/>' +
      wing(-1) + wing(1) +
      '<ellipse cx="30" cy="43" rx="12.5" ry="9" fill="url(#' + g + 'b)"' + ln() + '/>' +
      '<ellipse cx="30" cy="45" rx="7.5" ry="6" fill="' + p.l + '"/>' +
      '<path d="M26.5 41.5 h7 M26 44.5 h8 M27 47.5 h6" stroke="' + p.s + '" stroke-width=".7" opacity=".45"/>' +
      '<ellipse cx="23.5" cy="51.5" rx="4" ry="2.4" fill="' + p.b + '"' + ln() + '/><ellipse cx="36.5" cy="51.5" rx="4" ry="2.4" fill="' + p.b + '"' + ln() + '/>' +
      '<path d="M15 16 C10 11 9 6 11 3 C13 8 16 10 19 12 Z" fill="' + p.h + '"' + ln() + '/>' +
      '<path d="M45 16 C50 11 51 6 49 3 C47 8 44 10 41 12 Z" fill="' + p.h + '"' + ln() + '/>' +
      '<path d="M12 22 C6 21 3 24 2 27 C6 26 9 27 12 27 Z" fill="' + p.w + '"' + ln() + '/>' +
      '<path d="M48 22 C54 21 57 24 58 27 C54 26 51 27 48 27 Z" fill="' + p.w + '"' + ln() + '/>' +
      '<ellipse cx="30" cy="23.5" rx="18.5" ry="15" fill="url(#' + g + 'h)"' + ln(1.1) + '/>' +
      '<path d="M26 9.4 l2 -4 l2 4 M30 9 l2.2 -3.4 l1.6 3.8" fill="' + p.s + '"' + ln(.8) + '/>' +
      eyes(p, o, 22, 38, 23.5, 4.6, 5.4, true) +
      blush(17, 43, 29.5) +
      '<circle cx="27.6" cy="30" r=".75" fill="' + K + '"/><circle cx="32.4" cy="30" r=".75" fill="' + K + '"/>' +
      mouth(o, 32.5);
  }

  function cat(p, o, g){
    return '<path d="M41 49 C51 50 55 44 53 37 C52 33 48 34 49 37 C50 42 47 44 41 44 Z" fill="' + p.b + '"' + ln() + '/>' +
      '<ellipse cx="30" cy="43.5" rx="12" ry="8.5" fill="url(#' + g + 'b)"' + ln() + '/>' +
      '<ellipse cx="30" cy="45.5" rx="7" ry="5.5" fill="' + p.l + '"/>' +
      '<ellipse cx="24" cy="51.5" rx="4" ry="2.4" fill="' + p.l + '"' + ln() + '/><ellipse cx="36" cy="51.5" rx="4" ry="2.4" fill="' + p.l + '"' + ln() + '/>' +
      '<path d="M13 19 L12 4 L25 11 Z" fill="' + p.b + '"' + ln(1.1) + '/><path d="M15 15 L14.5 8 L21 11.5 Z" fill="' + p.i + '"/>' +
      '<path d="M47 19 L48 4 L35 11 Z" fill="' + p.b + '"' + ln(1.1) + '/><path d="M45 15 L45.5 8 L39 11.5 Z" fill="' + p.i + '"/>' +
      '<ellipse cx="30" cy="23.5" rx="18" ry="14.5" fill="url(#' + g + 'h)"' + ln(1.1) + '/>' +
      '<ellipse cx="30" cy="30" rx="6.5" ry="4.2" fill="' + p.l + '" opacity=".9"/>' +
      eyes(p, o, 22.5, 37.5, 22.5, 4, 4.8, true) +
      blush(17.5, 42.5, 28.5) +
      '<path d="M28.4 27.4 h3.2 l-1.6 1.8 z" fill="' + p.n + '"' + ln(.7) + '/>' +
      mouth(o, 29.6) +
      '<path d="M18 28 l-8 -1.2 M18 30 l-8 1.2 M42 28 l8 -1.2 M42 30 l8 1.2" stroke="' + K + '" stroke-width=".7" stroke-linecap="round" opacity=".55"/>';
  }

  function fox(p, o, g){
    return '<path d="M40 48 C52 50 57 42 54 34 C53 31 50 32 50 35 C51 40 47 42 40 42 Z" fill="' + p.b + '"' + ln() + '/>' +
      '<path d="M53.4 32.6 C55 34 55.6 36 55 38 C53.6 37.4 51.6 36.4 50.4 35 C50.6 33.4 51.8 32.4 53.4 32.6 Z" fill="' + p.l + '"' + ln(.8) + '/>' +
      '<ellipse cx="30" cy="43.5" rx="12" ry="8.5" fill="url(#' + g + 'b)"' + ln() + '/>' +
      '<path d="M24 37 Q30 50 36 37 Q30 41 24 37 Z" fill="' + p.l + '"/>' +
      '<ellipse cx="24" cy="51.5" rx="4" ry="2.4" fill="' + p.t + '"' + ln() + '/><ellipse cx="36" cy="51.5" rx="4" ry="2.4" fill="' + p.t + '"' + ln() + '/>' +
      '<path d="M12 20 L10 2 L26 11 Z" fill="' + p.b + '"' + ln(1.1) + '/><path d="M11.5 9 L10.6 3 L16 6 Z" fill="' + p.t + '"/><path d="M14 16 L13 8 L21 12 Z" fill="' + p.l + '" opacity=".85"/>' +
      '<path d="M48 20 L50 2 L34 11 Z" fill="' + p.b + '"' + ln(1.1) + '/><path d="M48.5 9 L49.4 3 L44 6 Z" fill="' + p.t + '"/><path d="M46 16 L47 8 L39 12 Z" fill="' + p.l + '" opacity=".85"/>' +
      '<path d="M30 9 C42 9 49 15 49 22 C49 27 46 30 42 31 C38 34 34 36 30 36 C26 36 22 34 18 31 C14 30 11 27 11 22 C11 15 18 9 30 9 Z" fill="url(#' + g + 'h)"' + ln(1.1) + '/>' +
      '<path d="M12 24 C16 30 22 32 30 36 C38 32 44 30 48 24 C44 27 38 27 34 26 C32 26 31 27 30 28 C29 27 28 26 26 26 C22 27 16 27 12 24 Z" fill="' + p.l + '"/>' +
      eyes(p, o, 22.5, 37.5, 21.5, 3.4, 4, false) +
      '<ellipse cx="30" cy="28.6" rx="2" ry="1.4" fill="' + p.n + '"/>' +
      mouth(o, 31);
  }

  function svg(kind, color, opts){
    const o = opts || {};
    const list = COLORS[kind] || COLORS.dragon;
    const p = list[color] || list[0];
    const g = 'pg' + (++uid);
    const draw = kind === 'cat' ? cat : kind === 'fox' ? fox : dragon;
    return '<svg xmlns="http://www.w3.org/2000/svg" width="' + W + '" height="' + H + '" viewBox="0 0 60 55">' +
      '<defs><radialGradient id="' + g + 'h" cx=".38" cy=".3" r=".8"><stop offset="0" stop-color="#ffffff" stop-opacity=".28"/><stop offset=".45" stop-color="' + p.b + '"/><stop offset="1" stop-color="' + p.s + '"/></radialGradient>' +
      '<radialGradient id="' + g + 'b" cx=".4" cy=".3" r=".9"><stop offset="0" stop-color="' + p.b + '"/><stop offset="1" stop-color="' + p.s + '"/></radialGradient></defs>' +
      draw(p, o, g) + '</svg>';
  }
  window.PET = {svg, COLORS, NAMES, W, H};
})();
