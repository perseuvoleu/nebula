export function InstallApp() {
  return (
    <details className="px-3 py-3 text-xs text-muted-foreground">
      <summary className="cursor-pointer py-1 font-medium text-foreground">Instalează pe telefon</summary>
      <p className="mt-2 leading-relaxed">iPhone: deschide în Safari → Partajare → „Adăugați pe ecranul principal” → Adăugați. Dacă apare opțiunea, păstrează „Deschideți ca aplicație web” activată.</p>
      <p className="mt-2 leading-relaxed">Android: Chrome → meniul ⋮ → „Instalează aplicația” sau „Adaugă pe ecranul de pornire”.</p>
      <p className="mt-2 leading-relaxed">La prima deschidere a aplicației instalate introdu același token de acces.</p>
    </details>
  );
}
