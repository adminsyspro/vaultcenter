// Code 39 symbol table (standard encodings, also documented by JsBarcode):
// https://github.com/lindell/JsBarcode/blob/master/src/barcodes/CODE39/index.js
const alphabet = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-';
const symbols = [20957,29783,23639,30485,20951,29813,23669,20855,29789,23645,29975,23831,30533,22295,30149,24005,21623,29981,23837,22301,30023,23879,30545,22343,30161,24017,21959,30065,23921,22385,29015,18263,29141,17879,29045,18293,17783];
const guard = '100010111011101';
const colors = ['#f49383','#f3ec39','#93b63e','#58d4d7','#c3c5c6','#f49a48','#ed8bbe','#b8d888','#f0bb99','#b6a3d9'];

export function LtoBarcodeLabel({barcode}:{barcode:string}) {
 const suffix=barcode.match(/L[0-9A-Z]$/)?.[0]??'';
 const serial=suffix?barcode.slice(0,-2):barcode;
 const supported=[...barcode].every(character=>alphabet.includes(character));
 const encoded=supported?[guard,...[...barcode].map(character=>symbols[alphabet.indexOf(character)].toString(2)),guard].join('0'):'';
 const bars=[];
 for(let offset=0;offset<encoded.length;){
  if(encoded[offset]==='0'){offset++;continue;}
  const start=offset;while(encoded[offset]==='1')offset++;
  bars.push(<rect key={start} x={start+10} y="0" width={offset-start} height="24"/>);
 }
 return <span className="lto-barcode-label" role="img" aria-label={`Code-barres ${barcode}`}>
  <span className="lto-color-code" aria-hidden="true" style={{fontSize:serial.length>10?'.65rem':'.875rem'}}>
   {[...serial].map((character,index)=><span key={index} style={{backgroundColor:colors[(alphabet.indexOf(character)+10)%10]}}>{character}</span>)}
   {suffix&&<span className="lto-media-code">{suffix}</span>}
  </span>
  <svg className="lto-barcode" viewBox={`0 0 ${encoded.length+20} 24`} preserveAspectRatio="none" aria-hidden="true" focusable="false">{bars}</svg>
 </span>;
}
