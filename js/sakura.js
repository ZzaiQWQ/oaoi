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
