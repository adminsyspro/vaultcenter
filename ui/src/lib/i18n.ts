"use client";
import {useSyncExternalStore} from 'react';
import core from './locales/en-core.ts';
import media from './locales/en-media.ts';
import storage from './locales/en-storage.ts';
import admin from './locales/en-admin.ts';
import api from './locales/en-api.ts';
import controllers from './locales/en-controllers.ts';

export type Language = 'fr'|'en';
export const LANGUAGE_STORAGE_KEY = 'vaultcenter-language';
const english:Record<string,string> = {...core,...media,...storage,...admin,...api,...controllers};
const french = new Map(Object.entries(english).map(([fr,en])=>[en,fr]));
// Recognize messages already stored in component state, including their original
// parameters, so changing language never requires resetting a form or operation.
const patterns = Object.entries(english).filter(([fr,en])=>fr!==en).flatMap(([canonical, translated]) =>
 [canonical, translated].filter(value => /\{\w+\}/.test(value)).map(value => {
  const keys:string[] = [];
  const escaped = value.split(/(\{\w+\})/).map(part => {
   if (/^\{\w+\}$/.test(part)) { const name=part.slice(1,-1); keys.push(name); return name==='number'?'([0-9]+)':'([\\s\\S]*?)'; }
   return part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }).join('');
  return {canonical, keys, expression:new RegExp('^'+escaped+'$'), literal:value.replace(/\{\w+\}/g,'').length};
 }))
 // The most specific pattern wins: « {title} de {barcode} » must not swallow a longer sentence.
 .sort((left,right)=>right.literal-left.literal);
const aliases:Record<string,string> = {
 'authentication required':'Authentification requise.',
 'permission denied':'Vous n’avez pas les droits nécessaires.',
 'invalid current password':'Le mot de passe actuel est incorrect.',
};
let language:Language = 'fr';
const listeners = new Set<()=>void>();
const subscribe = (listener:()=>void) => {listeners.add(listener);return()=>{listeners.delete(listener);};};
export const getLanguage = () => language;
export const getLocale = ():'fr-FR'|'en-GB' => language==='en'?'en-GB':'fr-FR';
export const isPlural = (count:number):boolean => new Intl.PluralRules(getLocale()).select(count)!=='one';
export function preferredLanguage(saved:string|null, browserLanguages:readonly string[]):Language {
 if(saved==='fr'||saved==='en')return saved;
 const first=browserLanguages[0]?.toLowerCase().split('-')[0];
 return first==='en'?'en':'fr';
}
export function setLanguage(value:Language) {
 if(value!=='fr'&&value!=='en')return;
 if(typeof window!=='undefined'){
  try{localStorage.setItem(LANGUAGE_STORAGE_KEY,value);}catch{/* The selection still works with browser storage disabled. */}
  document.documentElement.lang=value;
 }
 if(language===value)return;
 language=value;listeners.forEach(listener=>listener());
}
export function initializeLanguage() {
 let saved:string|null=null;try{saved=localStorage.getItem(LANGUAGE_STORAGE_KEY);}catch{/* Storage can be unavailable. */}
 setLanguage(preferredLanguage(saved,navigator.languages?.length?navigator.languages:[navigator.language]));
 const synchronize=(event:StorageEvent)=>{if(event.key===LANGUAGE_STORAGE_KEY&&(event.newValue==='fr'||event.newValue==='en'))setLanguage(event.newValue);};
 window.addEventListener('storage',synchronize);return()=>window.removeEventListener('storage',synchronize);
}
export function translate(message:string, params:Record<string,string|number> = {}):string {
 let canonical=Object.hasOwn(english,message)?message:french.get(message)??aliases[message];
 let values=params;
 if(canonical===undefined){
  for(const {canonical:key,keys,expression} of patterns){
   const match=expression.exec(message);
   if(!match)continue;
   const captured:Record<string,string>={};
   let consistent=true;
   keys.forEach((name,index)=>{if(Object.hasOwn(captured,name)&&captured[name]!==match[index+1])consistent=false;captured[name]=match[index+1];});
   if(!consistent)continue;
   // A backend sentence nested in a pattern ({message}, {reason}) is translated on its own.
   for(const name of ['message','reason'])if(Object.hasOwn(captured,name))captured[name]=translate(captured[name]);
   canonical=key;values={...captured,...params};break;
  }
 }
 canonical??=message;
 const template=language==='en'?(english[canonical]??canonical):canonical;
 return template.replace(/\{([a-zA-Z0-9_]+)\}/g,(match,key)=>Object.hasOwn(values,key)?String(values[key]):match);
}
export function useI18n() {
 const current=useSyncExternalStore(subscribe,getLanguage,()=> 'fr' as Language);
 return {language:current,locale:current==='en'?'en-GB' as const:'fr-FR' as const,t:translate,setLanguage};
}
