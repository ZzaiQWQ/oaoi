// ============ 樱花特效 ============

class SakuraPetals {
  constructor(container, count = 15) {
    this.container = container;
    this.count = count;
    this.paused = false;
    this.destroyed = false;
    this._timers = [];
    this.init();
  }

  init() {
    for (let i = 0; i < this.count; i++) {
      const t = setTimeout(() => this.createPetal(), i * 400);
      this._timers.push(t);
    }
  }

  /** 彻底销毁实例，停止所有递归创建并移除已有花瓣 */
  destroy() {
    this.destroyed = true;
    this._timers.forEach(t => clearTimeout(t));
    this._timers = [];
  }

  createPetal() {
    if (this.destroyed) return;
    if (this.paused) {
      const t = setTimeout(() => this.createPetal(), 1000);
      this._timers.push(t);
      return;
    }
    const petal = document.createElement('div');
    petal.className = 'sakura-petal';
    const styleMap = {
      sakura: ['🌸', '✿', '❀', '💮'],
      leaf:   ['🍃', '🍂', '🌿', '☘️'],
      snow:   ['❄️', '❅', '❆', '✦'],
      star:   ['⭐', '✨', '💫', '🌟']
    };
    const activeStyle = this.container.dataset.petalStyle || 'sakura';
    const activePetals = styleMap[activeStyle] || styleMap.sakura;
    petal.textContent = activePetals[Math.floor(Math.random() * activePetals.length)];

    const startX = Math.random() * 100;
    const size = 12 + Math.random() * 14;
    const duration = 8 + Math.random() * 10;
    const delay = Math.random() * 3;
    const swayAmount = 40 + Math.random() * 80;

    petal.style.cssText = `
      left: ${startX}%;
      font-size: ${size}px;
      animation-duration: ${duration}s;
      animation-delay: ${delay}s;
      opacity: ${0.4 + Math.random() * 0.4};
      will-change: transform;
    `;
    petal.style.setProperty('--sway', `${swayAmount}px`);
    this.container.appendChild(petal);

    petal.addEventListener('animationend', () => {
      petal.remove();
      if (!this.destroyed) this.createPetal();
    });
  }
}

function createPetalBurst(element) {
  const rect = element.getBoundingClientRect();
  const centerX = rect.left + rect.width / 2;
  const centerY = rect.top + rect.height / 2;

  for (let i = 0; i < 12; i++) {
    const petal = document.createElement('div');
    petal.textContent = '🌸';
    petal.style.cssText = `
      position: fixed;
      left: ${centerX}px;
      top: ${centerY}px;
      font-size: ${14 + Math.random() * 10}px;
      pointer-events: none;
      z-index: 1000;
      transition: all ${0.6 + Math.random() * 0.6}s cubic-bezier(0.25, 0.46, 0.45, 0.94);
      opacity: 1;
    `;

    document.body.appendChild(petal);

    requestAnimationFrame(() => {
      const angle = (Math.PI * 2 * i) / 12;
      const distance = 60 + Math.random() * 80;
      petal.style.transform = `translate(${Math.cos(angle) * distance}px, ${Math.sin(angle) * distance}px) rotate(${Math.random() * 360}deg) scale(0.3)`;
      petal.style.opacity = '0';
    });

    setTimeout(() => petal.remove(), 1200);
  }
}
